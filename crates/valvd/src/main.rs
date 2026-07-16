use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
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
use tracing_subscriber::{filter::LevelFilter, prelude::*, EnvFilter};
use valv_sync::{
    persistence::{mounts as mount_store, open_db},
    protocol::ipc::{AccountStatus, MountStatus, PrincipalStatus},
};

mod config;
mod control;
mod error;
mod fp;
#[cfg(target_os = "macos")]
mod launchd;
mod mounts;
mod nodes;
mod path_resolution;
mod restore;
mod self_update;
#[cfg(target_os = "linux")]
mod systemd;
mod tasks;

use config::{
    config_path, data_dir, load_config, merge_config_mounts, socket_path, tcp_port_file_path,
    DaemonConfig, MountConfig,
};
#[cfg(target_os = "macos")]
use launchd::{install_daemon, uninstall_daemon};
#[cfg(target_os = "linux")]
use systemd::{install_daemon, uninstall_daemon};
use tasks::{cancel_mount_tasks, spawn_account_status_task, spawn_mount_tasks, spawn_update_check_task, UpdateStatus};

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
    account: Arc<Mutex<Option<AccountStatus>>>,
    principal: Arc<Mutex<Option<PrincipalStatus>>>,
    device_token_rejected: Arc<AtomicBool>,
    update_status: Arc<Mutex<UpdateStatus>>,
    backend_health: Arc<BackendHealth>,
    pending_uploads: Arc<Mutex<HashSet<String>>>,
    deferred_deletes: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    db: Arc<Mutex<Connection>>,
    client: reqwest::Client,
    config: DaemonConfig,
}

#[derive(Debug, Default)]
struct BackendHealth {
    last_success_at: AtomicI64,
    last_failure_at: AtomicI64,
}

impl BackendHealth {
    fn record_success(&self) {
        let now = current_unix_seconds();
        let last_failure = self.last_failure_at.load(Ordering::Acquire);
        self.last_success_at
            .store(now.max(last_failure), Ordering::Release);
    }

    fn record_failure(&self) {
        let now = current_unix_seconds();
        let last_success = self.last_success_at.load(Ordering::Acquire);
        self.last_failure_at
            .store(now.max(last_success + 1), Ordering::Release);
    }

    fn is_connected(&self) -> bool {
        self.last_success_at.load(Ordering::Acquire) >= self.last_failure_at.load(Ordering::Acquire)
    }
}

fn current_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
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
    update_required: bool,
    update_required_flag: Arc<AtomicBool>,
    rejected: Arc<AtomicBool>,
    error: Option<String>,
    watcher_alive: Arc<AtomicBool>,
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
        Command::Daemon { command } => {
            let result = match command {
                DaemonCommand::Install => install_daemon(),
                DaemonCommand::Uninstall => uninstall_daemon(),
            };
            if let Err(error) = result {
                eprintln!("{error}");
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    #[cfg(target_os = "macos")]
    {
        let stdout_layer = tracing_subscriber::fmt::layer();
        let log_dir = match config::log_dir() {
            Ok(log_dir) => log_dir,
            Err(error) => {
                eprintln!("failed to resolve valvd log directory: {error}");
                tracing_subscriber::registry()
                    .with(filter)
                    .with(stdout_layer)
                    .init();
                return;
            }
        };
        if let Err(error) = fs::create_dir_all(&log_dir)
            .and_then(|_| config::prune_old_logs(&log_dir).map_err(std::io::Error::other))
        {
            eprintln!("failed to prepare valvd log directory: {error}");
        }
        let file_appender = tracing_appender::rolling::daily(&log_dir, "valvd.log");
        let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
        Box::leak(Box::new(guard));
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_writer);
        tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux intentionally keeps journald as the only persistent log store for
        // systemd user units; adding a second in-process file sink would duplicate
        // journal capture and rotation rather than solve the macOS launchd gap.
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }
}

async fn run() -> Result<()> {
    self_update::reap_stale_backup();
    let config_file = config_path()?;
    let config = load_config(&config_file)?;
    let db_path = data_dir()?.join("sync.db");
    let conn = open_db(&db_path)?;
    normalize_persisted_mount_paths(&conn)?;
    merge_config_mounts(&conn, &canonicalize_config_mount_paths(&config.mounts))?;
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
            update_required: false,
            update_required_flag: Arc::new(AtomicBool::new(false)),
            rejected: Arc::new(AtomicBool::new(false)),
            error: None,
            watcher_alive: Arc::new(AtomicBool::new(true)),
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
        account: Arc::new(Mutex::new(None)),
        principal: Arc::new(Mutex::new(None)),
        device_token_rejected: Arc::new(AtomicBool::new(false)),
        update_status: Arc::new(Mutex::new(UpdateStatus::default())),
        backend_health: Arc::new(BackendHealth::default()),
        pending_uploads: Arc::new(Mutex::new(HashSet::new())),
        deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
        db: Arc::new(Mutex::new(conn)),
        client: reqwest::Client::new(),
        config,
    };
    spawn_mount_tasks(&state).await;
    let _account_status_task = spawn_account_status_task(&state);
    // VALV_NO_UPDATE_CHECK=1 disables this task entirely at startup (checked
    // once here, not a live toggle) so smoke/e2e runs never make a live
    // GitHub API call (daemon-lifecycle capability).
    let no_update_check = std::env::var("VALV_NO_UPDATE_CHECK").ok();
    let _update_check_task = if tasks::should_spawn_update_check(no_update_check.as_deref()) {
        Some(spawn_update_check_task(&state))
    } else {
        None
    };

    serve_socket(state, &socket_path()?, &config_file, mount_count).await
}

fn normalize_persisted_mount_paths(conn: &Connection) -> Result<()> {
    for mount in mount_store::list_mounts(conn)? {
        let canonical = path_resolution::normalize_path(&mount.path)
            .to_string_lossy()
            .into_owned();
        if canonical == mount.path {
            continue;
        }
        if let Some(existing) = mount_store::get_mount(conn, &canonical)? {
            if mount.mount_token.is_some() && existing.mount_token.is_none() {
                tracing::warn!(
                    path = %mount.path,
                    canonical = %canonical,
                    kept_folder_id = %mount.folder_id,
                    dropped_folder_id = %existing.folder_id,
                    "two mount rows canonicalize to the same path; keeping the one that holds the mount credential"
                );
                mount_store::delete_mount(conn, &canonical)?;
                mount_store::update_mount_path(conn, &mount.path, &canonical)?;
            } else {
                tracing::warn!(
                    path = %mount.path,
                    canonical = %canonical,
                    kept_folder_id = %existing.folder_id,
                    dropped_folder_id = %mount.folder_id,
                    "duplicate mount row canonicalizes to an existing path; dropping the non-canonical duplicate"
                );
                mount_store::delete_mount(conn, &mount.path)?;
            }
            continue;
        }
        mount_store::update_mount_path(conn, &mount.path, &canonical)?;
    }
    Ok(())
}

fn canonicalize_config_mount_paths(mounts: &[MountConfig]) -> Vec<MountConfig> {
    mounts
        .iter()
        .map(|mount| MountConfig {
            path: path_resolution::normalize_path(&mount.path)
                .to_string_lossy()
                .into_owned(),
            ..mount.clone()
        })
        .collect()
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
        .route("/fp/move", post(fp::fp_move))
        .route("/fp/share", post(fp::fp_share))
        .route("/fp/watch", get(fp::fp_watch))
        .route("/nodes/:node_id/path", get(nodes::get_node_path))
        .route("/nodes/by-path", get(nodes::get_node_by_path))
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
    pub(crate) fn effective_token<'a>(&'a self, config: &'a DaemonConfig) -> Option<&'a str> {
        self.mount_token
            .as_deref()
            .or_else(|| config.device_token.as_deref())
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
            update_required: self.update_required
                || self.update_required_flag.load(Ordering::Acquire),
            error: self.error.clone(),
            watcher_alive: self.watcher_alive.load(Ordering::Acquire),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn backend_health_uses_latest_recorded_outcome() {
        let health = BackendHealth::default();
        assert!(health.is_connected());

        health.record_failure();
        health.record_success();
        assert!(health.is_connected());

        health.record_failure();
        assert!(!health.is_connected());
    }

    fn test_state() -> DaemonState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(Vec::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            principal: Arc::new(Mutex::new(None)),
            device_token_rejected: Arc::new(AtomicBool::new(false)),
            update_status: Arc::new(Mutex::new(Default::default())),
            backend_health: Arc::new(BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: Some("token-1".to_owned()),
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

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        conn
    }

    #[test]
    fn normalize_persisted_mount_paths_canonicalizes_a_symlinked_ancestor() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let non_canonical = link.to_string_lossy().into_owned();
        mount_store::upsert_mount(&conn, &non_canonical, "folder-1", None, None, None, true)
            .unwrap();

        normalize_persisted_mount_paths(&conn).unwrap();

        let canonical = real.canonicalize().unwrap().to_string_lossy().into_owned();
        assert!(mount_store::get_mount(&conn, &canonical).unwrap().is_some());
        assert!(mount_store::get_mount(&conn, &non_canonical).unwrap().is_none());
    }

    #[test]
    fn normalize_persisted_mount_paths_leaves_an_already_canonical_path_untouched() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap().to_string_lossy().into_owned();
        mount_store::upsert_mount(&conn, &canonical, "folder-1", None, None, None, true).unwrap();

        normalize_persisted_mount_paths(&conn).unwrap();

        let mounts = mount_store::list_mounts(&conn).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].path, canonical);
    }

    #[test]
    fn normalize_persisted_mount_paths_drops_duplicate_when_canonical_row_already_exists() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap().to_string_lossy().into_owned();
        let non_canonical = format!("{canonical}/.");
        mount_store::upsert_mount(&conn, &canonical, "folder-canonical", None, None, None, true)
            .unwrap();
        mount_store::upsert_mount(&conn, &non_canonical, "folder-stale", None, None, None, true)
            .unwrap();

        normalize_persisted_mount_paths(&conn).unwrap();

        let mounts = mount_store::list_mounts(&conn).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].folder_id, "folder-canonical");
    }

    #[test]
    fn normalize_persisted_mount_paths_keeps_the_credential_bearing_duplicate() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canonical = real.canonicalize().unwrap().to_string_lossy().into_owned();
        let non_canonical = link.to_string_lossy().into_owned();
        mount_store::upsert_mount(&conn, &canonical, "folder-tokenless", None, None, None, true)
            .unwrap();
        mount_store::upsert_mount(
            &conn,
            &non_canonical,
            "folder-with-token",
            None,
            None,
            Some("tok"),
            true,
        )
        .unwrap();

        normalize_persisted_mount_paths(&conn).unwrap();

        let mounts = mount_store::list_mounts(&conn).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].folder_id, "folder-with-token");
        assert_eq!(mounts[0].mount_token.as_deref(), Some("tok"));
        assert_eq!(mounts[0].path, canonical);
    }

    #[test]
    fn canonicalize_config_mount_paths_absolutizes_a_relative_path_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        fs::create_dir_all("relative-mount").unwrap();

        let canonicalized = canonicalize_config_mount_paths(&[MountConfig {
            path: "relative-mount".to_owned(),
            folder_id: "folder-1".to_owned(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
        }]);

        std::env::set_current_dir(original_cwd).unwrap();
        assert!(Path::new(&canonicalized[0].path).is_absolute());
    }

    #[test]
    fn a_non_canonical_persisted_row_is_not_duplicated_by_a_matching_config_entry_after_normalization(
    ) {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let non_canonical = link.to_string_lossy().into_owned();
        mount_store::upsert_mount(&conn, &non_canonical, "folder-existing", None, None, None, true)
            .unwrap();

        normalize_persisted_mount_paths(&conn).unwrap();
        let config_mounts = canonicalize_config_mount_paths(&[MountConfig {
            path: non_canonical,
            folder_id: "folder-stale".to_owned(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
        }]);
        merge_config_mounts(&conn, &config_mounts).unwrap();

        let mounts = mount_store::list_mounts(&conn).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].folder_id, "folder-existing");
    }
}
