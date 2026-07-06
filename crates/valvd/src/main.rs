use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use axum::{
    routing::{get, post},
    Router,
};
use clap::{Parser, Subcommand};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperBuilder,
    service::TowerToHyperService,
};
use rusqlite::Connection;
use tokio::{
    net::{TcpListener, UnixListener},
    signal,
    sync::{Mutex, Notify},
    task::JoinHandle,
};
use tracing_subscriber::{filter::LevelFilter, EnvFilter};
use valv_sync::{
    persistence::{mounts as mount_store, open_db},
    protocol::ipc::MountStatus,
};

mod config;
mod control;
mod error;
mod fp;
#[cfg(target_os = "macos")]
mod launchd;
mod mounts;
mod nodes;
mod restore;
#[cfg(target_os = "linux")]
mod systemd;
mod tasks;

use config::{
    config_path, data_dir, load_config, merge_config_mounts, socket_path, tcp_port_file_path,
    DaemonConfig,
};
#[cfg(target_os = "macos")]
use launchd::{install_daemon, uninstall_daemon};
#[cfg(target_os = "linux")]
use systemd::{install_daemon, uninstall_daemon};
use tasks::{cancel_mount_tasks, spawn_mount_tasks};

#[derive(Parser)]
#[command(name = "valvd", about = "Valv sync daemon", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run,
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    Install,
    Uninstall,
}

#[derive(Clone)]
struct DaemonState {
    paused: Arc<AtomicBool>,
    fs_events_paused: Arc<AtomicBool>,
    mounts: Arc<Mutex<Vec<MountState>>>,
    // Keyed by mount path so mounting/remounting one folder only cancels and
    // respawns that mount's own tasks instead of every persisted mount's.
    tasks: Arc<Mutex<HashMap<String, Vec<JoinHandle<()>>>>>,
    db: Arc<Mutex<Connection>>,
    client: reqwest::Client,
    config: DaemonConfig,
}

#[derive(Debug, Clone)]
struct MountState {
    path: String,
    folder_id: String,
    grant_id: Option<String>,
    scope_node_id: Option<String>,
    mount_token: Option<String>,
    can_write: bool,
    name: String,
    active_syncs: u32,
    pending_ops: u64,
    last_synced_at: Option<String>,
    error: Option<String>,
    // Serializes pull_mount_once/full_sync_mount for this mount so a background
    // pull can't mutate the local mirror mid-flight through an explicit sync's
    // push_local pass (see oss/crates/valv-sync/src/sync_engine/local_push.rs).
    sync_lock: Arc<Mutex<()>>,
    // Wakes any pending GET /fp/watch long-poll for this mount after its cursor
    // advances. Shared (not per-clone) so every MountState clone for the same
    // mount notifies the same waiters; unrelated to sync_lock, which guards a
    // different concern (mutual exclusion of mirror-mutating work).
    cursor_notify: Arc<Notify>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    match Cli::parse().command {
        Command::Run => run().await,
        Command::Daemon { command } => match command {
            DaemonCommand::Install => install_daemon(),
            DaemonCommand::Uninstall => uninstall_daemon(),
        },
    }
}

fn init_tracing() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn run() -> Result<()> {
    let config_file = config_path()?;
    let config = load_config(&config_file)?;
    let db_path = data_dir()?.join("sync.db");
    let conn = open_db(&db_path)?;
    merge_config_mounts(&conn, &config.mounts)?;
    let mount_states = mount_store::list_mounts(&conn)?
        .into_iter()
        .map(|mount| MountState {
            path: mount.path,
            folder_id: mount.folder_id,
            grant_id: mount.grant_id,
            scope_node_id: mount.scope_node_id,
            mount_token: mount.mount_token,
            can_write: mount.can_write,
            name: mount.name.unwrap_or_default(),
            active_syncs: 0,
            pending_ops: 0,
            last_synced_at: None,
            error: None,
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(Notify::new()),
        })
        .collect::<Vec<_>>();
    let mount_count = mount_states.len();
    let state = DaemonState {
        paused: Arc::new(AtomicBool::new(false)),
        fs_events_paused: Arc::new(AtomicBool::new(false)),
        mounts: Arc::new(Mutex::new(mount_states)),
        tasks: Arc::new(Mutex::new(HashMap::new())),
        db: Arc::new(Mutex::new(conn)),
        client: reqwest::Client::new(),
        config,
    };
    spawn_mount_tasks(&state).await;

    serve_socket(state, &socket_path()?, &config_file, mount_count).await
}

async fn serve_socket(
    state: DaemonState,
    socket_path: &Path,
    config_path: &Path,
    mount_count: usize,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind daemon socket {}", socket_path.display()))?;

    let tcp_listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind daemon TCP loopback listener")?;
    let tcp_addr = tcp_listener.local_addr()?;
    write_tcp_port_file(tcp_addr.port())?;

    tracing::info!(
        socket_path = %socket_path.display(),
        tcp_bind_addr = %tcp_addr,
        config_path = %config_path.display(),
        mount_count,
        "valvd startup summary"
    );

    let app = build_router(state.clone());

    tokio::select! {
        result = accept_loop_unix(listener, app.clone()) => result,
        result = accept_loop_tcp(tcp_listener, app) => result,
        result = shutdown_signal() => {
            state.paused.store(true, Ordering::Release);
            cancel_mount_tasks(&state).await;
            if let Err(err) = fs::remove_file(socket_path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err.into());
                }
            }
            result
        }
    }
}

// Shared by the real Unix-socket/TCP-loopback listeners and by tests, so the two
// listeners (and any test asserting they agree) can never drift into serving
// different route sets.
fn build_router(state: DaemonState) -> Router {
    Router::new()
        .route("/status", get(control::get_status))
        .route("/mounts", get(control::get_mounts))
        .route(
            "/mount",
            post(mounts::post_mount).delete(mounts::delete_mount_route),
        )
        .route("/pause", post(control::post_pause))
        .route("/resume", post(control::post_resume))
        .route("/sync", post(tasks::post_sync))
        .route("/versions", post(restore::post_versions))
        .route("/restore", post(restore::post_restore))
        .route("/fp/items", get(fp::fp_items))
        .route("/fp/item/:node_id", get(fp::fp_item))
        .route("/fp/anchor", get(fp::fp_anchor))
        .route("/fp/changes", get(fp::fp_changes))
        .route("/fp/content/:node_id", get(fp::fp_content))
        .route("/fp/upload", post(fp::fp_upload))
        .route("/fp/delete", post(fp::fp_delete).delete(fp::fp_delete))
        .route("/fp/share", post(fp::fp_share))
        .route("/fp/watch", get(fp::fp_watch))
        .route("/nodes/:node_id/path", get(nodes::get_node_path))
        .with_state(state)
}

// Advertises the TCP loopback port to the sandboxed macOS Xcode targets via the shared
// app-group container (see `config::tcp_port_file_path`). Best-effort on non-macOS/CI
// environments where the Group Containers path may not be meaningful: log and continue
// rather than fail daemon startup over a macOS-only convenience file.
fn write_tcp_port_file(port: u16) -> Result<()> {
    let path = tcp_port_file_path()?;
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                error = %err,
                "could not create TCP port file parent directory"
            );
            return Ok(());
        }
    }
    if let Err(err) = fs::write(&path, port.to_string()) {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "could not write TCP port file"
        );
    }
    Ok(())
}

async fn accept_loop_unix(listener: UnixListener, app: Router) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            if let Err(err) = HyperBuilder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
            {
                if err.to_string().contains("shutting down") {
                    tracing::debug!(error = %err, "daemon socket connection shutting down");
                } else {
                    tracing::warn!(error = %err, "daemon socket connection failed");
                }
            }
        });
    }
}

async fn accept_loop_tcp(listener: TcpListener, app: Router) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            if let Err(err) = HyperBuilder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
            {
                if err.to_string().contains("shutting down") {
                    tracing::debug!(error = %err, "daemon TCP connection shutting down");
                } else {
                    tracing::warn!(error = %err, "daemon TCP connection failed");
                }
            }
        });
    }
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    signal::ctrl_c().await?;
    Ok(())
}

impl MountState {
    pub(crate) fn effective_token<'a>(&'a self, config: &'a DaemonConfig) -> &'a str {
        self.mount_token.as_deref().unwrap_or(&config.device_token)
    }

    pub(crate) fn status(&self) -> MountStatus {
        MountStatus {
            path: self.path.clone(),
            folder_id: self.folder_id.clone(),
            name: self.name.clone(),
            scope_node_id: self.scope_node_id.clone(),
            grant_id: self.grant_id.clone(),
            can_write: self.can_write,
            syncing: self.active_syncs > 0,
            pending_ops: self.pending_ops,
            last_synced_at: self.last_synced_at.clone(),
            error: self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn test_state() -> DaemonState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(Vec::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: "token-1".to_owned(),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    // Sends a raw `GET /status HTTP/1.1` request over an already-connected stream and
    // returns the response's status line, mirroring the minimal hand-rolled framing
    // DaemonKit's Swift `DaemonClient` will also use against this same server.
    async fn get_status_line<S: AsyncReadExt + AsyncWriteExt + Unpin>(stream: &mut S) -> String {
        stream
            .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        text.lines().next().unwrap_or_default().to_owned()
    }

    #[tokio::test]
    async fn unix_and_tcp_listeners_serve_identical_status_responses() {
        // /tmp directly, not std::env::temp_dir() - on macOS the latter resolves to a
        // long per-process path that can exceed AF_UNIX's SUN_LEN limit (~104 bytes).
        let socket_dir = Path::new("/tmp").join(format!(
            "valvd-test-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        fs::create_dir_all(&socket_dir).unwrap();
        let socket_path = socket_dir.join("valvd.sock");

        let unix_listener = UnixListener::bind(&socket_path).unwrap();
        let tcp_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let tcp_port = tcp_listener.local_addr().unwrap().port();

        let app = build_router(test_state());
        tokio::spawn(accept_loop_unix(unix_listener, app.clone()));
        tokio::spawn(accept_loop_tcp(tcp_listener, app));

        let mut unix_stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let unix_status = get_status_line(&mut unix_stream).await;

        let mut tcp_stream = tokio::net::TcpStream::connect(("127.0.0.1", tcp_port))
            .await
            .unwrap();
        let tcp_status = get_status_line(&mut tcp_stream).await;

        assert_eq!(unix_status, "HTTP/1.1 200 OK");
        assert_eq!(unix_status, tcp_status);

        let _ = fs::remove_dir_all(&socket_dir);
    }
}
