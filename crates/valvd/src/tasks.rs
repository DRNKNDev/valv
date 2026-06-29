use std::{collections::HashMap, fs, path::PathBuf, sync::atomic::Ordering, time::Duration};

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use serde::Deserialize;
use tokio::{
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};
use valv_sync::{
    persistence::versions::{upsert_version, LocalVersion},
    protocol::{
        ipc::{SyncRequest, SyncSummary},
        sync::{ChunkRef, WsPushNotification},
    },
    storage::download_chunks,
    sync_engine::{
        delta_pull::pull_delta,
        local_push::{push_local, PushSummary},
        ws_client::ws_push_loop,
    },
    watch::{fs_watch_task, WatchMount},
};

use crate::{internal_error, DaemonState, ErrorResponse, MountState};

pub(crate) async fn post_sync(
    State(state): State<DaemonState>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<SyncSummary>, (StatusCode, Json<ErrorResponse>)> {
    let targets = {
        let mounts = state.mounts.lock().await;
        mounts
            .iter()
            .filter(|mount| {
                req.folder_id
                    .as_ref()
                    .is_none_or(|folder_id| folder_id == &mount.folder_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    let mut summary = SyncSummary::default();
    for mount in targets {
        let mount_summary = run_full_sync_mount(state.clone(), mount)
            .await
            .map_err(internal_error)?;
        merge_sync_summary(&mut summary, mount_summary);
    }

    Ok(Json(summary))
}

pub(crate) async fn spawn_mount_tasks(state: &DaemonState) {
    let mounts = state.mounts.lock().await.clone();
    for mount in mounts {
        spawn_tasks_for_mount(state, mount).await;
    }
}

pub(crate) async fn spawn_tasks_for_mount(state: &DaemonState, mount: MountState) {
    let (notify_tx, notify_rx) = mpsc::channel::<WsPushNotification>(32);
    let token = mount.effective_token(&state.config).to_owned();

    let sync_state = state.clone();
    let sync_mount = mount.clone();
    let sync_handle = tokio::spawn(async move {
        sync_loop(sync_state, sync_mount, notify_rx).await;
    });

    let ws_backend_url = state.config.backend_url.clone();
    let ws_token = token.clone();
    let ws_folder_id = mount.folder_id.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(error) =
            ws_push_loop(&ws_backend_url, &ws_token, vec![ws_folder_id], notify_tx).await
        {
            eprintln!("websocket task failed: {error}");
        }
    });

    let fs_handle = tokio::spawn({
        let paused = state.paused.clone();
        let db = state.db.clone();
        let client = state.client.clone();
        let backend_url = state.config.backend_url.clone();
        let device_name = state.config.device_name.clone();
        let watch_mount = WatchMount {
            path: PathBuf::from(&mount.path),
            folder_id: mount.folder_id.clone(),
            device_name,
        };
        async move {
            if let Err(error) =
                fs_watch_task(watch_mount, paused, db, client, backend_url, token).await
            {
                eprintln!("filesystem watch task failed: {error}");
            }
        }
    });

    state
        .tasks
        .lock()
        .await
        .extend([sync_handle, ws_handle, fs_handle]);
}

async fn sync_loop(
    state: DaemonState,
    mount: MountState,
    mut notify_rx: mpsc::Receiver<WsPushNotification>,
) {
    let mut ticker = interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {},
            notification = notify_rx.recv() => {
                let Some(notification) = notification else {
                    return;
                };
                if notification.folder_id != mount.folder_id {
                    continue;
                }
            }
        }

        if state.paused.load(Ordering::Acquire) {
            continue;
        }

        pull_mount_once(&state, &mount).await;
    }
}

async fn pull_mount_once(state: &DaemonState, mount: &MountState) {
    set_mount_syncing(state, &mount.folder_id, true, None).await;
    let result = {
        let mut conn = state.db.lock().await;
        let token = mount.effective_token(&state.config).to_owned();
        pull_delta(
            &state.client,
            &state.config.backend_url,
            &token,
            &mount.folder_id,
            &mut conn,
        )
        .await
    };
    let error = result.err().map(|err| err.to_string());
    set_mount_syncing(state, &mount.folder_id, false, error).await;
}

async fn full_sync_mount(state: &DaemonState, mount: &MountState) -> SyncSummary {
    set_mount_syncing(state, &mount.folder_id, true, None).await;

    let mut summary = SyncSummary::default();
    let push_result = push_local(
        PathBuf::from(&mount.path).as_path(),
        &mount.folder_id,
        mount.scope_node_id.as_deref(),
        &state.db,
        &state.client,
        &state.config.backend_url,
        mount.effective_token(&state.config),
        &state.config.device_name,
    )
    .await;
    match push_result {
        Ok(push_summary) => {
            merge_push_summary(&mut summary, &push_summary);
            set_mount_pending_ops(
                state,
                &mount.folder_id,
                push_summary.creates_submitted + push_summary.versions_submitted,
            )
            .await;
        }
        Err(error) => {
            eprintln!("push_local failed for {}: {error}", mount.folder_id);
            summary.errors += 1;
        }
    }

    let pull_result = {
        let mut conn = state.db.lock().await;
        let token = mount.effective_token(&state.config).to_owned();
        pull_delta(
            &state.client,
            &state.config.backend_url,
            &token,
            &mount.folder_id,
            &mut conn,
        )
        .await
    };
    let error = match pull_result {
        Ok(pulled_ops) => {
            summary.pulled_ops = pulled_ops;
            if let Err(error) = materialize_mount_files(state, mount).await {
                eprintln!("materialize files failed for {}: {error}", mount.folder_id);
                summary.errors += 1;
            }
            None
        }
        Err(error) => {
            summary.errors += 1;
            Some(error.to_string())
        }
    };

    set_mount_pending_ops(state, &mount.folder_id, 0).await;
    set_mount_syncing(state, &mount.folder_id, false, error).await;
    summary
}

#[derive(Debug)]
struct MaterializeNode {
    node_id: String,
    parent_id: Option<String>,
    name: String,
    node_type: String,
    current_version_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteVersion {
    version_id: String,
    content_hash: String,
    size_bytes: u64,
    manifest: Vec<ChunkRef>,
}

async fn materialize_mount_files(state: &DaemonState, mount: &MountState) -> anyhow::Result<()> {
    let nodes = {
        let conn = state.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, name, node_type, current_version_id
             FROM nodes
             WHERE folder_id = ?1 AND deleted_at IS NULL
             ORDER BY parent_id IS NOT NULL, name ASC",
        )?;
        let rows = stmt.query_map([&mount.folder_id], |row| {
            Ok(MaterializeNode {
                node_id: row.get(0)?,
                parent_id: row.get(1)?,
                name: row.get(2)?,
                node_type: row.get(3)?,
                current_version_id: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let Some(root) = nodes.iter().find(|node| node.parent_id.is_none()) else {
        return Ok(());
    };

    let mut paths = HashMap::from([(root.node_id.clone(), PathBuf::from(&mount.path))]);
    fs::create_dir_all(&mount.path)?;

    for node in nodes.iter().filter(|node| node.parent_id.is_some()) {
        let Some(parent_path) = node.parent_id.as_ref().and_then(|parent_id| paths.get(parent_id)) else {
            continue;
        };
        let path = parent_path.join(&node.name);
        paths.insert(node.node_id.clone(), path.clone());

        if node.node_type == "folder" {
            fs::create_dir_all(path)?;
            continue;
        }

        let Some(version_id) = node.current_version_id.as_deref() else {
            continue;
        };
        let version = fetch_remote_version(state, mount, &node.node_id, version_id).await?;
        {
            let conn = state.db.lock().await;
            upsert_version(
                &conn,
                &LocalVersion {
                    version_id: version.version_id.clone(),
                    node_id: node.node_id.clone(),
                    folder_id: mount.folder_id.clone(),
                    content_hash: version.content_hash.clone(),
                    size_bytes: version.size_bytes,
                    manifest_json: serde_json::to_string(&version.manifest)?,
                },
            )?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let token = mount.effective_token(&state.config).to_owned();
        let bytes = download_chunks(
            &state.client,
            &state.config.backend_url,
            &token,
            &version.manifest,
        )
        .await?;
        fs::write(path, bytes)?;
    }

    Ok(())
}

async fn fetch_remote_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version_id: &str,
) -> anyhow::Result<RemoteVersion> {
    let token = mount.effective_token(&state.config).to_owned();
    let versions = state
        .client
        .get(format!(
            "{}/folders/{}/versions/{}",
            valv_sync::api_base(&state.config.backend_url),
            mount.folder_id,
            node_id,
        ))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<RemoteVersion>>()
        .await?;
    versions
        .into_iter()
        .find(|version| version.version_id == version_id)
        .ok_or_else(|| anyhow::anyhow!("version {version_id} not found for node {node_id}"))
}

async fn run_full_sync_mount(
    state: DaemonState,
    mount: MountState,
) -> Result<SyncSummary, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(full_sync_mount(&state, &mount))
    })
    .await
}

pub(crate) async fn cancel_mount_tasks(state: &DaemonState) {
    for task in state.tasks.lock().await.drain(..) {
        task.abort();
    }
}

async fn set_mount_syncing(
    state: &DaemonState,
    folder_id: &str,
    syncing: bool,
    error: Option<String>,
) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.syncing = syncing;
        mount.error = error;
        if !syncing && mount.error.is_none() {
            mount.last_synced_at = Some(Utc::now().to_rfc3339());
        }
    }
}

async fn set_mount_pending_ops(state: &DaemonState, folder_id: &str, pending_ops: u64) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.pending_ops = pending_ops;
    }
}

fn merge_push_summary(summary: &mut SyncSummary, push_summary: &PushSummary) {
    summary.creates_submitted += push_summary.creates_submitted;
    summary.versions_submitted += push_summary.versions_submitted;
    summary.errors += push_summary.errors;
}

fn merge_sync_summary(summary: &mut SyncSummary, mount_summary: SyncSummary) {
    summary.creates_submitted += mount_summary.creates_submitted;
    summary.versions_submitted += mount_summary.versions_submitted;
    summary.pulled_ops += mount_summary.pulled_ops;
    summary.errors += mount_summary.errors;
}
