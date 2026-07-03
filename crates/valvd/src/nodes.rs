use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use rusqlite::Connection;
use valv_sync::{persistence::nodes as node_store, protocol::ipc::NodePathResponse};

use crate::{internal_error, DaemonState, ErrorResponse};

pub(crate) async fn get_node_path(
    State(state): State<DaemonState>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<NodePathResponse>, (StatusCode, Json<ErrorResponse>)> {
    let conn = state.db.lock().await;
    let Some(node) = node_store::get_node(&conn, &node_id).map_err(internal_error)? else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("node_not_found")),
        ));
    };

    let scope_node_id = state
        .mounts
        .lock()
        .await
        .iter()
        .find(|mount| mount.folder_id == node.folder_id)
        .and_then(|mount| mount.scope_node_id.clone());

    let path = resolve_node_path(&conn, &node_id, scope_node_id.as_deref()).map_err(internal_error)?;
    Ok(Json(NodePathResponse { path }))
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
    use valv_sync::persistence::LocalNode;

    use super::*;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!(
            "../../valv-sync/src/persistence/schema.sql"
        ))
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
            db: Arc::new(Mutex::new(memory_db())),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: "token".to_owned(),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        };

        let (status, _) = get_node_path(State(state), AxumPath("unknown".to_owned()))
            .await
            .unwrap_err();

        assert_eq!(status, StatusCode::NOT_FOUND);
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
}
