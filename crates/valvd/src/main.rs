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
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::{Parser, Subcommand};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperBuilder,
    service::TowerToHyperService,
};
use rusqlite::Connection;
use serde::Serialize;
use tokio::{net::UnixListener, signal, sync::Mutex, task::JoinHandle};
use valv_sync::{
    persistence::{mounts as mount_store, open_db},
    protocol::ipc::MountStatus,
};

mod config;
mod control;
mod fp;
#[cfg(target_os = "macos")]
mod launchd;
mod mounts;
mod restore;
#[cfg(target_os = "linux")]
mod systemd;
mod tasks;

use config::{config_path, data_dir, load_config, merge_config_mounts, socket_path, DaemonConfig};
#[cfg(target_os = "macos")]
use launchd::{install_daemon, uninstall_daemon};
#[cfg(target_os = "linux")]
use systemd::{install_daemon, uninstall_daemon};
use tasks::{cancel_mount_tasks, spawn_mount_tasks};

#[derive(Parser)]
#[command(name = "valvd", about = "Valv sync daemon")]
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
    active_syncs: u32,
    pending_ops: u64,
    last_synced_at: Option<String>,
    error: Option<String>,
    // Serializes pull_mount_once/full_sync_mount for this mount so a background
    // pull can't mutate the local mirror mid-flight through an explicit sync's
    // push_local pass (see oss/crates/valv-sync/src/sync_engine/local_push.rs).
    sync_lock: Arc<Mutex<()>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run => run().await,
        Command::Daemon { command } => match command {
            DaemonCommand::Install => install_daemon(),
            DaemonCommand::Uninstall => uninstall_daemon(),
        },
    }
}

async fn run() -> Result<()> {
    let config = load_config(&config_path()?)?;
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
            active_syncs: 0,
            pending_ops: 0,
            last_synced_at: None,
            error: None,
            sync_lock: Arc::new(Mutex::new(())),
        })
        .collect();
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

    serve_socket(state, &socket_path()?).await
}

async fn serve_socket(state: DaemonState, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind daemon socket {}", socket_path.display()))?;
    let app = Router::new()
        .route("/status", get(control::get_status))
        .route("/mounts", get(control::get_mounts))
        .route("/mount", post(mounts::post_mount))
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
        .with_state(state.clone());

    tokio::select! {
        result = accept_loop(listener, app) => result,
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

async fn accept_loop(listener: UnixListener, app: Router) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            if let Err(err) = HyperBuilder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
            {
                if !err.to_string().contains("shutting down") {
                    eprintln!("daemon socket connection failed: {err}");
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
            scope_node_id: self.scope_node_id.clone(),
            grant_id: self.grant_id.clone(),
            syncing: self.active_syncs > 0,
            pending_ops: self.pending_ops,
            last_synced_at: self.last_synced_at.clone(),
            error: self.error.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ErrorResponse {
    error: String,
}

impl ErrorResponse {
    pub(crate) fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
        }
    }
}

pub(crate) fn internal_error(error: impl std::fmt::Display) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(error.to_string())),
    )
}
