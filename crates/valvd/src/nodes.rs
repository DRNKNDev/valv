use axum::{
    extract::{Path as AxumPath, Query, State},
    Json,
};
use rusqlite::Connection;
use valv_sync::{
    persistence::nodes as node_store,
    protocol::ipc::{NodeByPathQuery, NodeByPathResponse, NodePathResponse},
};

use crate::{
    error::DaemonError,
    path_resolution::{resolve_path_to_node, PathResolutionError},
    DaemonState,
};

pub(crate) async fn get_node_path(
    State(state): State<DaemonState>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<NodePathResponse>, DaemonError> {
    let conn = state.db.lock().await;
    let Some(node) = node_store::get_node(&conn, &node_id)? else {
        return Err(DaemonError::NotFound("node_not_found".to_owned()));
    };

    let scope_node_id = state
        .mounts
        .lock()
        .await
        .iter()
        .find(|mount| mount.folder_id == node.folder_id)
        .and_then(|mount| mount.scope_node_id.clone());

    let path = resolve_node_path(&conn, &node_id, scope_node_id.as_deref())?;
    Ok(Json(NodePathResponse { path }))
}

pub(crate) async fn get_node_by_path(
    State(state): State<DaemonState>,
    Query(query): Query<NodeByPathQuery>,
) -> Result<Json<NodeByPathResponse>, DaemonError> {
    if query.path.trim().is_empty() {
        return Err(DaemonError::BadRequest("path is required".to_owned()));
    }
    let mounts = state.mounts.lock().await.clone();
    let conn = state.db.lock().await;
    let resolved =
        resolve_path_to_node(&conn, &mounts, &query.path).map_err(|error| match error {
            PathResolutionError::NotInMount => DaemonError::NotFound("not_in_mount".to_owned()),
            PathResolutionError::NodeNotSynced => {
                DaemonError::NotFound("node_not_synced".to_owned())
            }
            PathResolutionError::Internal(error) => DaemonError::from(error),
        })?;
    Ok(Json(NodeByPathResponse {
        node_id: resolved.node_id,
    }))
}

/// Walks `parent_id` upward from `node_id`, collecting each ancestor's `name`,
/// stopping at the node whose `parent_id IS NULL` (the mount's root) or at
/// `scope_node_id` for a partial-scope mount - matching `node_abs_path`'s
/// stopping rule in `tasks.rs`, but returning a `/`-joined display path
/// instead of a local filesystem `PathBuf`. The root/scope node itself
/// resolves to `""`.
fn resolve_node_path(
    conn: &Connection,
    node_id: &str,
    scope_node_id: Option<&str>,
) -> anyhow::Result<String> {
    let mut segments = Vec::new();
    let mut current_id = node_id.to_owned();
    loop {
        let Some(node) = node_store::get_node(conn, &current_id)? else {
            break;
        };
        if scope_node_id == Some(node.node_id.as_str()) || node.parent_id.is_none() {
            break;
        }
        segments.push(node.name);
        match node.parent_id {
            Some(parent_id) => current_id = parent_id,
            None => break,
        }
    }
    segments.reverse();
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        sync::{atomic::AtomicBool, Arc},
    };

    use tokio::sync::{Mutex, Notify};
    use uuid::Uuid;
    use valv_sync::persistence::{mounts as mount_store, LocalNode};

    use crate::{config::DaemonConfig, MountState};

    use super::*;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        conn
    }

    fn node(node_id: &str, parent_id: Option<&str>, name: &str) -> LocalNode {
        LocalNode {
            node_id: node_id.to_owned(),
            folder_id: "folder-1".to_owned(),
            parent_id: parent_id.map(str::to_owned),
            name: name.to_owned(),
            node_type: "folder".into(),
            current_version_id: None,
            server_seq: 0,
            deleted_at: None,
            pushed_size_bytes: None,
            pushed_mtime_nanos: None,
        }
    }

    #[test]
    fn root_resolves_to_empty_path() {
        let conn = memory_db();
        node_store::upsert_node(&conn, &node("root", None, "")).unwrap();

        let path = resolve_node_path(&conn, "root", None).unwrap();

        assert_eq!(path, "");
    }

    #[test]
    fn nested_node_resolves_to_joined_path() {
        let conn = memory_db();
        node_store::upsert_node(&conn, &node("root", None, "")).unwrap();
        node_store::upsert_node(&conn, &node("drafts", Some("root"), "Drafts")).unwrap();
        node_store::upsert_node(&conn, &node("q3", Some("drafts"), "Q3")).unwrap();

        let path = resolve_node_path(&conn, "q3", None).unwrap();

        assert_eq!(path, "Drafts/Q3");
    }

    #[tokio::test]
    async fn unknown_node_returns_404() {
        use std::{
            collections::HashMap,
            sync::{atomic::AtomicBool, Arc},
        };

        use tokio::sync::Mutex;

        use crate::config::DaemonConfig;

        let state = DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(Vec::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            principal: Arc::new(Mutex::new(None)),
            device_token_rejected: Arc::new(AtomicBool::new(false)),
            update_status: Arc::new(Mutex::new(Default::default())),
            backend_health: Arc::new(crate::BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(std::collections::HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(memory_db())),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: Some("token".to_owned()),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        };

        let error = get_node_path(State(state), AxumPath("unknown".to_owned()))
            .await
            .unwrap_err();

        assert!(matches!(error, DaemonError::NotFound(_)));
    }

    #[test]
    fn scoped_mount_stops_at_scope_node_id_not_the_folder_root() {
        let conn = memory_db();
        node_store::upsert_node(&conn, &node("root", None, "")).unwrap();
        node_store::upsert_node(&conn, &node("drafts", Some("root"), "Drafts")).unwrap();
        node_store::upsert_node(&conn, &node("q3", Some("drafts"), "Q3")).unwrap();

        // A mount scoped to "drafts" SHALL stop there, not continue up to
        // "root" (which is above the caller's granted scope).
        let path = resolve_node_path(&conn, "q3", Some("drafts")).unwrap();

        assert_eq!(path, "Q3");
    }

    fn file_node(node_id: &str, parent_id: Option<&str>, name: &str) -> LocalNode {
        LocalNode {
            node_id: node_id.to_owned(),
            folder_id: "folder-1".to_owned(),
            parent_id: parent_id.map(str::to_owned),
            name: name.to_owned(),
            node_type: "file".to_owned(),
            current_version_id: None,
            server_seq: 1,
            deleted_at: None,
            pushed_size_bytes: None,
            pushed_mtime_nanos: None,
        }
    }

    fn by_path_state(
        mount_path: String,
        scope_node_id: Option<String>,
        nodes: &[LocalNode],
    ) -> DaemonState {
        let mount = MountState {
            path: mount_path.clone(),
            folder_id: "folder-1".to_owned(),
            grant_id: None,
            scope_node_id,
            mount_token: None,
            can_write: true,
            name: "Test Folder".to_owned(),
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
        };
        let conn = memory_db();
        mount_store::upsert_mount(&conn, &mount_path, "folder-1", None, None, None, true).unwrap();
        for node in nodes {
            node_store::upsert_node(&conn, node).unwrap();
        }
        DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(vec![mount])),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            principal: Arc::new(Mutex::new(None)),
            device_token_rejected: Arc::new(AtomicBool::new(false)),
            update_status: Arc::new(Mutex::new(Default::default())),
            backend_health: Arc::new(crate::BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(std::collections::HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: Some("token".to_owned()),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("valvd-nodes-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn root_level_file_resolves_without_scope_node_id() {
        let mount_dir = temp_dir("root");
        let file_path = mount_dir.join("report.pdf");
        fs::write(&file_path, b"content").unwrap();
        let state = by_path_state(
            mount_dir.to_string_lossy().to_string(),
            None,
            &[
                file_node("root", None, ""),
                file_node("report", Some("root"), "report.pdf"),
            ],
        );

        let response = get_node_by_path(
            State(state),
            Query(NodeByPathQuery {
                path: file_path.to_string_lossy().to_string(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.node_id, "report");
        let _ = fs::remove_dir_all(mount_dir);
    }

    #[tokio::test]
    async fn nested_file_resolves_through_multiple_segments() {
        let mount_dir = temp_dir("nested");
        let nested_dir = mount_dir.join("Design Docs");
        fs::create_dir_all(&nested_dir).unwrap();
        let file_path = nested_dir.join("report.pdf");
        fs::write(&file_path, b"content").unwrap();
        let state = by_path_state(
            mount_dir.to_string_lossy().to_string(),
            None,
            &[
                file_node("root", None, ""),
                file_node("docs", Some("root"), "Design Docs"),
                file_node("report", Some("docs"), "report.pdf"),
            ],
        );

        let response = get_node_by_path(
            State(state),
            Query(NodeByPathQuery {
                path: file_path.to_string_lossy().to_string(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.node_id, "report");
        let _ = fs::remove_dir_all(mount_dir);
    }

    #[tokio::test]
    async fn path_outside_every_mount_returns_not_in_mount() {
        let mount_dir = temp_dir("mount");
        let outside_dir = temp_dir("outside");
        let outside_file = outside_dir.join("report.pdf");
        fs::write(&outside_file, b"content").unwrap();
        let state = by_path_state(
            mount_dir.to_string_lossy().to_string(),
            None,
            &[file_node("root", None, "")],
        );

        let error = get_node_by_path(
            State(state),
            Query(NodeByPathQuery {
                path: outside_file.to_string_lossy().to_string(),
            }),
        )
        .await
        .unwrap_err();

        match error {
            DaemonError::NotFound(message) => assert_eq!(message, "not_in_mount"),
            other => panic!("expected not_in_mount, got {other:?}"),
        }
        let _ = fs::remove_dir_all(mount_dir);
        let _ = fs::remove_dir_all(outside_dir);
    }

    #[tokio::test]
    async fn path_under_mount_with_no_matching_node_returns_node_not_synced() {
        let mount_dir = temp_dir("unsynced");
        let file_path = mount_dir.join("missing.txt");
        fs::write(&file_path, b"content").unwrap();
        let state = by_path_state(
            mount_dir.to_string_lossy().to_string(),
            None,
            &[file_node("root", None, "")],
        );

        let error = get_node_by_path(
            State(state),
            Query(NodeByPathQuery {
                path: file_path.to_string_lossy().to_string(),
            }),
        )
        .await
        .unwrap_err();

        match error {
            DaemonError::NotFound(message) => assert_eq!(message, "node_not_synced"),
            other => panic!("expected node_not_synced, got {other:?}"),
        }
        let _ = fs::remove_dir_all(mount_dir);
    }

    #[tokio::test]
    async fn symlinked_mount_path_resolves_identically() {
        let mount_dir = temp_dir("symlink-target");
        let file_path = mount_dir.join("report.pdf");
        fs::write(&file_path, b"content").unwrap();
        let symlink_dir =
            std::env::temp_dir().join(format!("valvd-nodes-symlink-{}", Uuid::new_v4()));
        std::os::unix::fs::symlink(&mount_dir, &symlink_dir).unwrap();
        let state = by_path_state(
            mount_dir.to_string_lossy().to_string(),
            None,
            &[
                file_node("root", None, ""),
                file_node("report", Some("root"), "report.pdf"),
            ],
        );

        let response = get_node_by_path(
            State(state),
            Query(NodeByPathQuery {
                path: symlink_dir.join("report.pdf").to_string_lossy().to_string(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.node_id, "report");
        let _ = fs::remove_file(&symlink_dir);
        let _ = fs::remove_dir_all(mount_dir);
    }
}
