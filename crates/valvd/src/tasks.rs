use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use serde::Deserialize;
use tokio::{
    sync::mpsc,
    time::{interval_at, sleep, Instant, MissedTickBehavior},
};
use valv_sync::{
    persistence::{
        chunks as chunk_store,
        nodes::LocalNode,
        versions::{self, upsert_version, LocalVersion},
    },
    protocol::{
        ipc::{SyncRequest, SyncSummary},
        sync::{ChunkRef, WsPushNotification},
    },
    storage::download_chunks,
    sync_engine::{
        delta_pull::{pull_delta, PulledNode},
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
        let fs_events_paused = state.fs_events_paused.clone();
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
            if let Err(error) = fs_watch_task(
                watch_mount,
                paused,
                fs_events_paused,
                db,
                client,
                backend_url,
                token,
            )
            .await
            {
                eprintln!("filesystem watch task failed: {error}");
            }
        }
    });

    state
        .tasks
        .lock()
        .await
        .insert(mount.path.clone(), vec![sync_handle, ws_handle, fs_handle]);
}

async fn sync_loop(
    state: DaemonState,
    mount: MountState,
    mut notify_rx: mpsc::Receiver<WsPushNotification>,
) {
    // interval_at (not interval) delays the first tick by a full period.
    // post_mount already runs tree_resync + materialize_mount_files before
    // this task is spawned, so an immediate first tick buys no correctness
    // benefit; it only means every mount on a daemon fires a redundant pull
    // the instant it's spawned. With many persisted mounts on one daemon
    // (e.g. a device that has mounted many folders over time), those pulls
    // all serialize on the daemon's single sync.db connection, which can
    // stall an unrelated foreground `valv sync` for many seconds.
    let period = Duration::from_secs(30);
    let mut ticker = interval_at(Instant::now() + period, period);
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
    let _sync_guard = mount.sync_lock.lock().await;
    begin_mount_sync(state, &mount.folder_id).await;
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
    let error = match result {
        Ok((_, pulled)) => {
            let was_paused = pause_watchers(state);
            let cleanup_error = cleanup_deleted_mount_paths(state, mount)
                .await
                .err()
                .map(|err| err.to_string());
            let apply_error = apply_pulled_fs_changes(state, mount, pulled)
                .await
                .err()
                .map(|err| err.to_string());
            resume_watchers_after_debounce(state, was_paused).await;
            apply_error.or(cleanup_error)
        }
        Err(err) => Some(err.to_string()),
    };
    let succeeded = error.is_none();
    end_mount_sync(state, &mount.folder_id, error).await;
    if succeeded {
        mount.cursor_notify.notify_waiters();
    }
}

async fn full_sync_mount(state: &DaemonState, mount: &MountState) -> SyncSummary {
    let _sync_guard = mount.sync_lock.lock().await;
    begin_mount_sync(state, &mount.folder_id).await;

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
        Ok((pulled_ops, pulled)) => {
            summary.pulled_ops = pulled_ops;
            let was_paused = pause_watchers(state);
            if let Err(error) = apply_pulled_fs_changes(state, mount, pulled).await {
                eprintln!(
                    "apply pulled filesystem changes failed for {}: {error}",
                    mount.folder_id
                );
                summary.errors += 1;
            }
            if let Err(error) = materialize_mount_files(state, mount).await {
                eprintln!("materialize files failed for {}: {error}", mount.folder_id);
                summary.errors += 1;
            }
            resume_watchers_after_debounce(state, was_paused).await;
            None
        }
        Err(error) => {
            summary.errors += 1;
            Some(error.to_string())
        }
    };

    set_mount_pending_ops(state, &mount.folder_id, 0).await;
    let pull_succeeded = error.is_none();
    end_mount_sync(state, &mount.folder_id, error).await;
    if pull_succeeded {
        mount.cursor_notify.notify_waiters();
    }
    summary
}

fn pause_watchers(state: &DaemonState) -> bool {
    state.fs_events_paused.swap(true, Ordering::AcqRel)
}

async fn resume_watchers_after_debounce(state: &DaemonState, was_paused: bool) {
    if !was_paused {
        sleep(Duration::from_millis(250)).await;
        state.fs_events_paused.store(false, Ordering::Release);
    }
}

#[derive(Debug)]
struct MaterializeNode {
    node_id: String,
    parent_id: Option<String>,
    name: String,
    node_type: String,
    current_version_id: Option<String>,
    deleted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteVersion {
    version_id: String,
    content_hash: String,
    size_bytes: u64,
    manifest: Vec<ChunkRef>,
}

pub fn node_abs_path(
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    scope_node_id: Option<&str>,
    node_id: &str,
) -> Option<PathBuf> {
    let mut segments = Vec::new();
    let mut current_id = node_id;
    loop {
        let node = nodes_by_id.get(current_id)?;
        if scope_node_id == Some(node.node_id.as_str()) || node.parent_id.is_none() {
            break;
        }
        segments.push(node.name.clone());
        current_id = node.parent_id.as_deref()?;
    }

    let mut path = mount_root.to_path_buf();
    for segment in segments.into_iter().rev() {
        path.push(segment);
    }
    Some(path)
}

async fn apply_pulled_fs_changes(
    state: &DaemonState,
    mount: &MountState,
    pulled: Vec<PulledNode>,
) -> anyhow::Result<()> {
    if pulled.is_empty() {
        return Ok(());
    }

    let nodes_by_id = load_nodes_by_id(state, &mount.folder_id).await?;
    let mount_root = PathBuf::from(&mount.path);
    for pulled_node in pulled {
        if let Err(error) =
            apply_pulled_fs_change(state, mount, &nodes_by_id, &mount_root, &pulled_node).await
        {
            eprintln!(
                "apply_pulled_fs_changes: failed to apply {} for {}: {error}",
                pulled_node.op_type, pulled_node.node_id
            );
        }
    }
    Ok(())
}

async fn load_nodes_by_id(
    state: &DaemonState,
    folder_id: &str,
) -> anyhow::Result<HashMap<String, LocalNode>> {
    let conn = state.db.lock().await;
    let mut stmt = conn.prepare(
        "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at
         FROM nodes
         WHERE folder_id = ?1",
    )?;
    let rows = stmt
        .query_map([folder_id], |row| {
            Ok(LocalNode {
                node_id: row.get(0)?,
                folder_id: row.get(1)?,
                parent_id: row.get(2)?,
                name: row.get(3)?,
                node_type: row.get(4)?,
                current_version_id: row.get(5)?,
                server_seq: row.get(6)?,
                deleted_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect())
}

async fn apply_pulled_fs_change(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    pulled: &PulledNode,
) -> anyhow::Result<()> {
    match pulled.op_type.as_str() {
        "create" if pulled.node_type == "folder" => {
            if let Some(path) = node_abs_path(
                nodes_by_id,
                mount_root,
                mount.scope_node_id.as_deref(),
                &pulled.node_id,
            ) {
                fs::create_dir_all(path)?;
            }
        }
        "create" if pulled.node_type == "file" => {
            if let Some(version_id) = pulled.new_version_id.as_deref() {
                write_canonical_version(state, mount, nodes_by_id, mount_root, pulled, version_id)
                    .await?;
            }
        }
        "rename" | "move" => {
            let Some(old_path) = pre_op_abs_path(nodes_by_id, mount_root, mount, pulled) else {
                return Ok(());
            };
            let Some(new_path) = node_abs_path(
                nodes_by_id,
                mount_root,
                mount.scope_node_id.as_deref(),
                &pulled.node_id,
            ) else {
                return Ok(());
            };
            if old_path == new_path {
                return Ok(());
            }
            if old_path.exists() {
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(old_path, new_path)?;
            } else {
                // old_path is already gone locally (e.g. a concurrent local
                // rename/delete raced this same node). We can't move it into
                // place, but the mirror DB now tracks this node at new_path
                // regardless, so materialize just this one node there.
                // Deliberately scoped to this single node rather than a full
                // materialize_mount_files sweep: that would also re-download
                // any other node the DB still tracks as live but which is
                // merely missing on disk because the user deleted it locally
                // and push_local (which would tell the server about that)
                // hasn't run yet.
                materialize_single_node(state, mount, nodes_by_id, &pulled.node_id, &new_path)
                    .await?;
            }
        }
        "new_version" if pulled.is_conflict_copy => {
            if pulled.actor_device_id == state.config.device_id {
                return Ok(());
            }
            let Some(version_id) = pulled.new_version_id.as_deref() else {
                return Ok(());
            };
            let Some(canonical_path) = node_abs_path(
                nodes_by_id,
                mount_root,
                mount.scope_node_id.as_deref(),
                &pulled.node_id,
            ) else {
                return Ok(());
            };
            let date = pulled
                .applied_at
                .split('T')
                .next()
                .unwrap_or(&pulled.applied_at);
            let conflict_path = conflict_copy_path(&canonical_path, &pulled.actor_device_id, date)?;
            if let Some(parent) = conflict_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let bytes =
                download_and_store_version(state, mount, &pulled.node_id, version_id).await?;
            fs::write(conflict_path, bytes)?;
        }
        "new_version" => {
            if pulled.old_version_id == pulled.new_version_id {
                return Ok(());
            }
            let Some(version_id) = pulled.new_version_id.as_deref() else {
                return Ok(());
            };
            write_canonical_version(state, mount, nodes_by_id, mount_root, pulled, version_id)
                .await?;
        }
        "delete" => {}
        _ => {}
    }
    Ok(())
}

/// Materializes a single tracked node at `path`, downloading file content if
/// needed. Unlike `materialize_mount_files`, this touches only the given
/// node rather than sweeping the whole tree, so it can safely be used from
/// background pull handling without risking resurrecting other nodes the
/// user has deleted locally but not yet pushed.
async fn materialize_single_node(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    node_id: &str,
    path: &Path,
) -> anyhow::Result<()> {
    let Some(node) = nodes_by_id.get(node_id) else {
        return Ok(());
    };
    if node.node_type == "folder" {
        fs::create_dir_all(path)?;
        return Ok(());
    }
    let Some(version_id) = node.current_version_id.as_deref() else {
        return Ok(());
    };
    let already_materialized = {
        let conn = state.db.lock().await;
        versions::get_version(&conn, version_id)?.is_some() && path.exists()
    };
    if already_materialized {
        return Ok(());
    }
    let bytes = download_and_store_version(state, mount, node_id, version_id).await?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn pre_op_abs_path(
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    mount: &MountState,
    pulled: &PulledNode,
) -> Option<PathBuf> {
    let mut old_nodes = nodes_by_id.clone();
    let node = old_nodes.get_mut(&pulled.node_id)?;
    if let Some(old_name) = &pulled.old_name {
        node.name = old_name.clone();
    }
    node.parent_id = pulled.old_parent_id.clone();
    node_abs_path(
        &old_nodes,
        mount_root,
        mount.scope_node_id.as_deref(),
        &pulled.node_id,
    )
}

async fn write_canonical_version(
    state: &DaemonState,
    mount: &MountState,
    nodes_by_id: &HashMap<String, LocalNode>,
    mount_root: &Path,
    pulled: &PulledNode,
    version_id: &str,
) -> anyhow::Result<()> {
    let Some(path) = node_abs_path(
        nodes_by_id,
        mount_root,
        mount.scope_node_id.as_deref(),
        &pulled.node_id,
    ) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = download_and_store_version(state, mount, &pulled.node_id, version_id).await?;
    fs::write(path, bytes)?;
    Ok(())
}

async fn download_and_store_version(
    state: &DaemonState,
    mount: &MountState,
    node_id: &str,
    version_id: &str,
) -> anyhow::Result<Vec<u8>> {
    let version = fetch_remote_version(state, mount, node_id, version_id).await?;
    {
        let conn = state.db.lock().await;
        upsert_version(
            &conn,
            &LocalVersion {
                version_id: version.version_id.clone(),
                node_id: node_id.to_owned(),
                folder_id: mount.folder_id.clone(),
                content_hash: version.content_hash.clone(),
                size_bytes: version.size_bytes,
                manifest_json: serde_json::to_string(&version.manifest)?,
            },
        )?;
    }
    let token = mount.effective_token(&state.config).to_owned();
    let bytes = download_chunks(
        &state.client,
        &state.config.backend_url,
        &token,
        &version.manifest,
    )
    .await?;
    {
        let conn = state.db.lock().await;
        for chunk in &version.manifest {
            chunk_store::mark_uploaded(&conn, &chunk.chunk_hash, chunk.length)?;
        }
    }
    Ok(bytes.to_vec())
}

fn conflict_copy_path(
    original_path: &Path,
    device_name: &str,
    date: &str,
) -> anyhow::Result<PathBuf> {
    let file_name = original_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!("path has no valid file name: {}", original_path.display())
        })?;
    let conflict_name = match original_path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => {
            let stem = original_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("path has no valid file stem: {}", original_path.display())
                })?;
            format!("{stem} (conflicted copy, {device_name}, {date}).{ext}")
        }
        None => format!("{file_name} (conflicted copy, {device_name}, {date})"),
    };
    Ok(original_path.with_file_name(conflict_name))
}

pub(crate) async fn materialize_mount_files(
    state: &DaemonState,
    mount: &MountState,
) -> anyhow::Result<()> {
    cleanup_deleted_mount_paths(state, mount).await?;

    let nodes = {
        let conn = state.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, name, node_type, current_version_id
             FROM nodes
             WHERE folder_id = ?1 AND deleted_at IS NULL
             ORDER BY parent_id IS NOT NULL, name ASC",
        )?;
        let rows = stmt
            .query_map([&mount.folder_id], |row| {
                Ok(MaterializeNode {
                    node_id: row.get(0)?,
                    parent_id: row.get(1)?,
                    name: row.get(2)?,
                    node_type: row.get(3)?,
                    current_version_id: row.get(4)?,
                    deleted_at: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let Some(root) = nodes.iter().find(|node| node.parent_id.is_none()) else {
        return Ok(());
    };

    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        if let Some(parent_id) = &node.parent_id {
            children_map.entry(parent_id.clone()).or_default().push(idx);
        }
    }

    let mut paths = HashMap::from([(root.node_id.clone(), PathBuf::from(&mount.path))]);
    fs::create_dir_all(&mount.path)?;

    let mut queue = VecDeque::new();
    queue.push_back(root.node_id.clone());

    while let Some(parent_id) = queue.pop_front() {
        let Some(child_indices) = children_map.get(&parent_id).cloned() else {
            continue;
        };
        for idx in child_indices {
            let node = &nodes[idx];
            let parent_path = paths[&parent_id].clone();
            let path = parent_path.join(&node.name);
            paths.insert(node.node_id.clone(), path.clone());
            queue.push_back(node.node_id.clone());

            if node.node_type == "folder" {
                fs::create_dir_all(&path)?;
                continue;
            }

            let Some(version_id) = node.current_version_id.as_deref() else {
                continue;
            };
            let already_materialized = {
                let conn = state.db.lock().await;
                versions::get_version(&conn, version_id)?.is_some() && path.exists()
            };
            if already_materialized {
                continue;
            }
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
            let token = mount.effective_token(&state.config).to_owned();
            let bytes = download_chunks(
                &state.client,
                &state.config.backend_url,
                &token,
                &version.manifest,
            )
            .await?;
            {
                let conn = state.db.lock().await;
                for chunk in &version.manifest {
                    chunk_store::mark_uploaded(&conn, &chunk.chunk_hash, chunk.length)?;
                }
            }
            fs::write(&path, bytes)?;
        }
    }

    Ok(())
}

async fn cleanup_deleted_mount_paths(
    state: &DaemonState,
    mount: &MountState,
) -> anyhow::Result<()> {
    let nodes = {
        let conn = state.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, name, node_type, current_version_id, deleted_at
             FROM nodes
             WHERE folder_id = ?1",
        )?;
        let rows = stmt
            .query_map([&mount.folder_id], |row| {
                Ok(MaterializeNode {
                    node_id: row.get(0)?,
                    parent_id: row.get(1)?,
                    name: row.get(2)?,
                    node_type: row.get(3)?,
                    current_version_id: row.get(4)?,
                    deleted_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let Some(root) = nodes.iter().find(|node| node.parent_id.is_none()) else {
        return Ok(());
    };

    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        if let Some(parent_id) = &node.parent_id {
            children_map.entry(parent_id.clone()).or_default().push(idx);
        }
    }

    let mut paths = HashMap::from([(root.node_id.clone(), PathBuf::from(&mount.path))]);
    let mut queue = VecDeque::new();
    let mut deleted_paths = Vec::new();
    queue.push_back(root.node_id.clone());

    while let Some(parent_id) = queue.pop_front() {
        let Some(child_indices) = children_map.get(&parent_id).cloned() else {
            continue;
        };
        for idx in child_indices {
            let node = &nodes[idx];
            let parent_path = paths[&parent_id].clone();
            let path = parent_path.join(&node.name);
            paths.insert(node.node_id.clone(), path.clone());
            queue.push_back(node.node_id.clone());

            if node.deleted_at.is_some() {
                deleted_paths.push((path, node.node_type.clone()));
            }
        }
    }

    deleted_paths.sort_by_key(|(path, _)| path.components().count());
    for (path, node_type) in deleted_paths.into_iter().rev() {
        let result = if node_type == "folder" {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(error) = result {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
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
    for (_, tasks) in state.tasks.lock().await.drain() {
        for task in tasks {
            task.abort();
        }
    }
}

pub(crate) async fn cancel_tasks_for_mount(state: &DaemonState, path: &str) {
    if let Some(tasks) = state.tasks.lock().await.remove(path) {
        for task in tasks {
            task.abort();
        }
    }
}

async fn begin_mount_sync(state: &DaemonState, folder_id: &str) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.active_syncs = mount.active_syncs.saturating_add(1);
        mount.error = None;
    }
}

async fn end_mount_sync(state: &DaemonState, folder_id: &str, error: Option<String>) {
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts.iter_mut().find(|mount| mount.folder_id == folder_id) {
        mount.active_syncs = mount.active_syncs.saturating_sub(1);
        mount.error = error;
        if mount.active_syncs == 0 && mount.error.is_none() {
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
