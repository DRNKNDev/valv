use axum::{extract::State, Json};
use reqwest::Response;
use serde::Deserialize;
use valv_sync::{
    api_base,
    persistence::nodes,
    protocol::{
        ipc::{RestoreRequest, RestoreResponse, VersionEntry, VersionsRequest, VersionsResponse},
        sync::SubmitOpResponse,
    },
};

use crate::{
    error::{backend_response_or_error, require_token, DaemonError},
    path_resolution::{resolve_path_to_node, PathResolutionError, ResolvedPath},
    DaemonState,
};

pub(crate) async fn post_versions(
    State(state): State<DaemonState>,
    Json(req): Json<VersionsRequest>,
) -> Result<Json<VersionsResponse>, DaemonError> {
    if req.local_path.trim().is_empty() {
        return Err(DaemonError::BadRequest("local_path is required".to_owned()));
    }
    let resolved = resolve_local_path(&state, &req.local_path).await?;
    let token = require_token(resolved.mount.effective_token(&state.config))?;
    let response = state
        .client
        .get(format!(
            "{}/folders/{}/nodes/{}/versions",
            api_base(&state.config.backend_url),
            resolved.folder_id,
            resolved.node_id,
        ))
        .bearer_auth(token)
        .send()
        .await?;
    let versions = parse_backend_json::<Vec<BackendVersionEntry>>(response)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();

    Ok(Json(VersionsResponse { versions }))
}

pub(crate) async fn post_restore(
    State(state): State<DaemonState>,
    Json(req): Json<RestoreRequest>,
) -> Result<Json<RestoreResponse>, DaemonError> {
    if req.local_path.trim().is_empty() {
        return Err(DaemonError::BadRequest("local_path is required".to_owned()));
    }
    if req.version_id.trim().is_empty() {
        return Err(DaemonError::BadRequest("version_id is required".to_owned()));
    }
    let resolved = resolve_local_path(&state, &req.local_path).await?;
    let based_on_seq = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &resolved.node_id)? else {
            return Err(DaemonError::NotFound(
                "path not found in local mirror".to_owned(),
            ));
        };
        node.server_seq
    };
    let token = require_token(resolved.mount.effective_token(&state.config))?;
    let response = state
        .client
        .post(format!(
            "{}/folders/{}/nodes/{}/versions/{}/restore",
            api_base(&state.config.backend_url),
            resolved.folder_id,
            resolved.node_id,
            req.version_id,
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "based_on_seq": based_on_seq }))
        .send()
        .await?;
    let response = parse_backend_json::<SubmitOpResponse>(response).await?;

    Ok(Json(RestoreResponse {
        result: response.result_str().to_owned(),
    }))
}

#[derive(Debug, Deserialize)]
struct BackendVersionEntry {
    version_id: String,
    created_at: String,
    size_bytes: u64,
    #[serde(default)]
    author_device_id: Option<String>,
    #[serde(default)]
    author_device_name: Option<String>,
    is_conflict_copy: bool,
}

impl From<BackendVersionEntry> for VersionEntry {
    fn from(entry: BackendVersionEntry) -> Self {
        Self {
            version_id: entry.version_id,
            created_at: entry.created_at,
            size_bytes: entry.size_bytes,
            author_device_name: entry
                .author_device_name
                .or(entry.author_device_id)
                .unwrap_or_else(|| "-".to_owned()),
            is_conflict_copy: entry.is_conflict_copy,
        }
    }
}

async fn resolve_local_path(
    state: &DaemonState,
    local_path: &str,
) -> Result<ResolvedPath, DaemonError> {
    let mounts = state.mounts.lock().await.clone();
    let conn = state.db.lock().await;
    resolve_path_to_node(&conn, &mounts, local_path).map_err(|error| match error {
        PathResolutionError::NotInMount => {
            DaemonError::NotFound("path is not under any mounted folder".to_owned())
        }
        PathResolutionError::NodeNotSynced => {
            DaemonError::NotFound("path not found in local mirror".to_owned())
        }
        PathResolutionError::Internal(error) => DaemonError::from(error),
    })
}

async fn parse_backend_json<T: for<'de> Deserialize<'de>>(
    response: Response,
) -> Result<T, DaemonError> {
    Ok(backend_response_or_error(response)
        .await?
        .json::<T>()
        .await?)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        sync::{atomic::AtomicBool, Arc},
    };

    use rusqlite::Connection;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{Mutex, Notify},
    };
    use uuid::Uuid;
    use valv_sync::{
        persistence::{mounts as mount_store, nodes as node_store, LocalNode},
        protocol::ipc::{RestoreRequest, VersionsRequest},
    };

    use crate::{config::DaemonConfig, MountState};

    use super::*;

    async fn backend_url_with_response(body: &'static str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn local_node(node_id: &str, parent_id: Option<&str>, name: &str) -> LocalNode {
        LocalNode {
            node_id: node_id.to_owned(),
            folder_id: "folder-1".to_owned(),
            parent_id: parent_id.map(str::to_owned),
            name: name.to_owned(),
            node_type: "file".to_owned(),
            current_version_id: None,
            server_seq: 4,
            deleted_at: None,
            pushed_size_bytes: None,
            pushed_mtime_nanos: None,
        }
    }

    fn test_state(mount_path: String, backend_url: String) -> DaemonState {
        let mount = MountState {
            path: mount_path.clone(),
            folder_id: "folder-1".to_owned(),
            grant_id: None,
            scope_node_id: None,
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        mount_store::upsert_mount(&conn, &mount_path, "folder-1", None, None, None, true).unwrap();
        node_store::upsert_node(&conn, &local_node("root", None, "")).unwrap();
        node_store::upsert_node(&conn, &local_node("doc", Some("root"), "doc.txt")).unwrap();
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
                backend_url,
                device_id: "device-1".to_owned(),
                device_token: Some("token".to_owned()),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn post_versions_success_returns_backend_versions() {
        let mount_dir = std::env::temp_dir().join(format!("valvd-restore-{}", Uuid::new_v4()));
        fs::create_dir_all(&mount_dir).unwrap();
        let file_path = mount_dir.join("doc.txt");
        fs::write(&file_path, b"content").unwrap();
        let backend_url = backend_url_with_response(
            r#"[{"version_id":"version-1","created_at":"2026-07-06T00:00:00Z","size_bytes":7,"author_device_name":"Alice","is_conflict_copy":false}]"#,
        )
        .await;

        let response = post_versions(
            State(test_state(
                mount_dir.to_string_lossy().to_string(),
                backend_url,
            )),
            Json(VersionsRequest {
                local_path: file_path.to_string_lossy().to_string(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.versions.len(), 1);
        assert_eq!(response.0.versions[0].version_id, "version-1");
        let _ = fs::remove_dir_all(mount_dir);
    }

    #[tokio::test]
    async fn post_versions_rejects_empty_local_path() {
        let error = post_versions(
            State(test_state(
                "/sync".to_owned(),
                "http://127.0.0.1:1".to_owned(),
            )),
            Json(VersionsRequest {
                local_path: String::new(),
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DaemonError::BadRequest(_)));
    }

    #[tokio::test]
    async fn post_restore_round_trips_conflict_copy_result() {
        let mount_dir = std::env::temp_dir().join(format!("valvd-restore-{}", Uuid::new_v4()));
        fs::create_dir_all(&mount_dir).unwrap();
        let file_path = mount_dir.join("doc.txt");
        fs::write(&file_path, b"content").unwrap();
        let backend_url = backend_url_with_response(
            r#"{"result":"conflict_copy","server_seq":9,"node_id":"doc","conflict_version_id":"conflict-1"}"#,
        )
        .await;

        let response = post_restore(
            State(test_state(
                mount_dir.to_string_lossy().to_string(),
                backend_url,
            )),
            Json(RestoreRequest {
                local_path: file_path.to_string_lossy().to_string(),
                version_id: "version-1".to_owned(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.result, "conflict_copy");
        let _ = fs::remove_dir_all(mount_dir);
    }
}
