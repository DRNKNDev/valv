use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};

use anyhow::Result;
use axum::{extract::State, http::StatusCode, Json};
use rusqlite::Connection;
use serde::Deserialize;
use tokio::sync::Mutex;
use valv_sync::{
    persistence::{mounts as mount_store, nodes},
    protocol::ipc::{MountRequest, MountResponse, UnmountRequest},
    sync_engine::delta_pull::tree_resync,
};

use crate::{
    error::{backend_response_or_error, DaemonError},
    tasks::{cancel_tasks_for_mount, materialize_mount_files, spawn_tasks_for_mount},
    DaemonState, MountState,
};

pub(crate) async fn post_mount(
    State(state): State<DaemonState>,
    Json(req): Json<MountRequest>,
) -> Result<Json<MountResponse>, DaemonError> {
    if req.path.trim().is_empty() {
        return Err(DaemonError::BadRequest("path is required".to_owned()));
    }
    if req.folder_id.as_deref().is_some_and(str::is_empty) {
        return Err(DaemonError::BadRequest(
            "folder_id cannot be empty".to_owned(),
        ));
    }
    if req.grant_token.as_deref().is_some_and(str::is_empty) {
        return Err(DaemonError::BadRequest(
            "grant_token cannot be empty".to_owned(),
        ));
    }
    if req.folder_id.is_some() && req.grant_token.is_some() {
        return Err(DaemonError::BadRequest(
            "folder_id_and_grant_token_are_mutually_exclusive".to_owned(),
        ));
    }

    let resolved = resolve_mount(&state, &req).await?;
    let token = resolved
        .mount_token
        .as_deref()
        .unwrap_or(&state.config.device_token)
        .to_owned();
    let local_name = {
        let mut conn = state.db.lock().await;
        mount_store::upsert_mount(
            &conn,
            &req.path,
            &resolved.folder_id,
            resolved.grant_id.as_deref(),
            resolved.scope_node_id.as_deref(),
            resolved.mount_token.as_deref(),
            resolved.can_write,
        )?;
        if let Err(err) = tree_resync(
            &state.client,
            &state.config.backend_url,
            &token,
            &resolved.folder_id,
            &mut conn,
        )
        .await
        {
            let _ = mount_store::delete_mount(&conn, &req.path);
            return Err(DaemonError::Internal(err.to_string()));
        }

        local_mount_name(&conn, &resolved)?
    };
    let name = match local_name {
        Some(name) => name,
        None => fetch_folder_name(&state, &resolved.folder_id, &token).await?,
    };
    {
        let conn = state.db.lock().await;
        mount_store::set_mount_name(&conn, &req.path, &name)?;
    }
    let mount = MountState {
        path: req.path.clone(),
        folder_id: resolved.folder_id.clone(),
        grant_id: resolved.grant_id.clone(),
        scope_node_id: resolved.scope_node_id.clone(),
        mount_token: resolved.mount_token,
        can_write: resolved.can_write,
        name,
        active_syncs: 0,
        pending_ops: 0,
        last_synced_at: None,
        update_required: false,
        update_required_flag: Arc::new(AtomicBool::new(false)),
        error: None,
        sync_lock: Arc::new(Mutex::new(())),
        cursor_notify: Arc::new(tokio::sync::Notify::new()),
    };
    if let Err(err) = materialize_mount_files(&state, &mount).await {
        let conn = state.db.lock().await;
        let _ = mount_store::delete_mount(&conn, &req.path);
        return Err(DaemonError::Internal(err.to_string()));
    }
    {
        let mut mounts = state.mounts.lock().await;
        if let Some(existing) = mounts
            .iter_mut()
            .find(|existing| existing.path == mount.path)
        {
            *existing = mount.clone();
        } else {
            mounts.push(mount.clone());
        }
    }
    cancel_tasks_for_mount(&state, &mount.path).await;
    spawn_tasks_for_mount(&state, mount).await;

    Ok(Json(MountResponse {
        folder_id: resolved.folder_id,
        grant_id: resolved.grant_id,
        scope_node_id: resolved.scope_node_id,
        path: req.path,
    }))
}

/// Unmounts a folder locally: stops its background sync tasks and removes its
/// `mounts` row. Deliberately does not delete the locally materialized files (the
/// user's data on disk) or call anything on the backend - the shared folder and its
/// grants are entirely unaffected, and so are any `nodes`/`versions`/`chunks` rows
/// left in the local mirror for that `folder_id` (harmless leftover cache data,
/// not a collision risk since `mounts.folder_id` is UNIQUE).
pub(crate) async fn delete_mount_route(
    State(state): State<DaemonState>,
    Json(req): Json<UnmountRequest>,
) -> Result<StatusCode, DaemonError> {
    if req.folder_id.trim().is_empty() {
        return Err(DaemonError::BadRequest("folder_id is required".to_owned()));
    }
    let mount = {
        let mounts = state.mounts.lock().await;
        mounts
            .iter()
            .find(|mount| mount.folder_id == req.folder_id)
            .cloned()
    };
    let Some(mount) = mount else {
        return Err(DaemonError::NotFound("mount_not_found".to_owned()));
    };

    cancel_tasks_for_mount(&state, &mount.path).await;
    {
        let conn = state.db.lock().await;
        mount_store::delete_mount(&conn, &mount.path)?;
    }
    {
        let mut mounts = state.mounts.lock().await;
        mounts.retain(|existing| existing.path != mount.path);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug)]
struct ResolvedMount {
    folder_id: String,
    grant_id: Option<String>,
    scope_node_id: Option<String>,
    mount_token: Option<String>,
    can_write: bool,
}

#[derive(Debug, Deserialize)]
struct CreateFolderResponse {
    folder_id: String,
}

#[derive(Debug, Deserialize)]
struct GrantListEntry {
    grant_id: String,
    folder_id: String,
    scope_node_id: String,
    can_write: bool,
}

#[derive(Debug, Deserialize)]
struct FolderResponse {
    name: String,
}

async fn resolve_mount(
    state: &DaemonState,
    req: &MountRequest,
) -> Result<ResolvedMount, DaemonError> {
    if let Some(grant_token) = &req.grant_token {
        let grants = state
            .client
            .get(format!(
                "{}/grants",
                valv_sync::api_base(&state.config.backend_url)
            ))
            .bearer_auth(grant_token)
            .send()
            .await?;
        let grants = backend_response_or_error(grants)
            .await?
            .json::<Vec<GrantListEntry>>()
            .await?;
        let grant = grants.into_iter().next().ok_or_else(|| {
            DaemonError::NotFound("grant token has no accessible grants".to_owned())
        })?;
        return Ok(ResolvedMount {
            folder_id: grant.folder_id,
            grant_id: Some(grant.grant_id),
            scope_node_id: Some(grant.scope_node_id),
            mount_token: Some(grant_token.clone()),
            can_write: grant.can_write,
        });
    }

    if let Some(folder_id) = &req.folder_id {
        return Ok(ResolvedMount {
            folder_id: folder_id.clone(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
            can_write: true,
        });
    }

    let name = Path::new(&req.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Valv Folder");
    let created = state
        .client
        .post(format!(
            "{}/folders",
            valv_sync::api_base(&state.config.backend_url)
        ))
        .bearer_auth(&state.config.device_token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await?;
    let created = backend_response_or_error(created)
        .await?
        .json::<CreateFolderResponse>()
        .await?;
    Ok(ResolvedMount {
        folder_id: created.folder_id,
        grant_id: None,
        scope_node_id: None,
        mount_token: None,
        can_write: true,
    })
}

/// Resolves the mount's display name from the local mirror alone, without any
/// network call. Returns `None` when the effective scope node's name is empty
/// (true of any folder's root node by construction), signaling the caller
/// should fall back to `fetch_folder_name`.
fn local_mount_name(conn: &Connection, resolved: &ResolvedMount) -> Result<Option<String>> {
    let effective_scope_node = match &resolved.scope_node_id {
        Some(scope_node_id) => nodes::get_node(conn, scope_node_id)?,
        None => nodes::get_root_node(conn, &resolved.folder_id)?,
    };
    Ok(effective_scope_node.and_then(|node| (!node.name.is_empty()).then_some(node.name)))
}

async fn fetch_folder_name(
    state: &DaemonState,
    folder_id: &str,
    token: &str,
) -> Result<String, DaemonError> {
    let folder = state
        .client
        .get(format!(
            "{}/folders/{}",
            valv_sync::api_base(&state.config.backend_url),
            folder_id
        ))
        .bearer_auth(token)
        .send()
        .await?;
    let folder = backend_response_or_error(folder)
        .await?
        .json::<FolderResponse>()
        .await?;
    Ok(folder.name)
}

#[cfg(test)]
mod tests {
    use valv_sync::persistence::{mounts as mount_store, nodes, LocalNode};

    use super::*;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        conn
    }

    fn folder_root_node(folder_id: &str, name: &str) -> LocalNode {
        LocalNode {
            node_id: format!("{folder_id}-root"),
            folder_id: folder_id.to_owned(),
            parent_id: None,
            name: name.to_owned(),
            node_type: "folder".into(),
            current_version_id: None,
            server_seq: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn whole_folder_mount_with_empty_root_name_needs_fallback() {
        let conn = memory_db();
        // A folder's root node always has an empty name by construction
        // (the real name lives in shared_folders.name), so a whole-folder
        // mount (no scope_node_id) SHALL signal that a GET /folders/:id
        // fallback is needed.
        nodes::upsert_node(&conn, &folder_root_node("folder-1", "")).unwrap();
        let resolved = ResolvedMount {
            folder_id: "folder-1".into(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
            can_write: true,
        };

        let name = local_mount_name(&conn, &resolved).unwrap();

        assert_eq!(name, None);
    }

    #[test]
    fn subfolder_scoped_mount_resolves_from_local_mirror_without_fallback() {
        let conn = memory_db();
        nodes::upsert_node(&conn, &folder_root_node("folder-1", "")).unwrap();
        nodes::upsert_node(
            &conn,
            &LocalNode {
                node_id: "node-drafts".into(),
                folder_id: "folder-1".into(),
                parent_id: Some("folder-1-root".into()),
                name: "Drafts".into(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 0,
                deleted_at: None,
            },
        )
        .unwrap();
        let resolved = ResolvedMount {
            folder_id: "folder-1".into(),
            grant_id: None,
            scope_node_id: Some("node-drafts".into()),
            mount_token: None,
            can_write: true,
        };

        // local_mount_name's signature takes no HTTP client at all, so a
        // `Some` result here is itself the proof no network call could have
        // been involved in resolving it.
        let name = local_mount_name(&conn, &resolved).unwrap();

        assert_eq!(name, Some("Drafts".to_owned()));
    }

    #[test]
    fn resolved_name_persists_and_reloads_without_re_resolution() {
        let conn = memory_db();
        mount_store::upsert_mount(&conn, "/sync", "folder-1", None, None, None, true).unwrap();
        mount_store::set_mount_name(&conn, "/sync", "Design Docs").unwrap();

        // Simulates a daemon restart: list_mounts reads the persisted name
        // straight from SQLite, with no name-resolution logic involved.
        let mounts = mount_store::list_mounts(&conn).unwrap();

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name.as_deref(), Some("Design Docs"));
    }

    fn test_mount(path: &str, folder_id: &str) -> MountState {
        MountState {
            path: path.to_owned(),
            folder_id: folder_id.to_owned(),
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
            error: None,
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn test_state(mounts: Vec<MountState>) -> DaemonState {
        let conn = memory_db();
        for mount in &mounts {
            mount_store::upsert_mount(
                &conn,
                &mount.path,
                &mount.folder_id,
                None,
                None,
                None,
                mount.can_write,
            )
            .unwrap();
        }
        DaemonState {
            paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fs_events_paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(mounts)),
            tasks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            backend_health: Arc::new(crate::BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(std::collections::HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config: crate::DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: "token".to_owned(),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn delete_mount_removes_only_the_targeted_mount() {
        let state = test_state(vec![
            test_mount("/sync-a", "folder-a"),
            test_mount("/sync-b", "folder-b"),
        ]);

        let response = delete_mount_route(
            axum::extract::State(state.clone()),
            axum::Json(UnmountRequest {
                folder_id: "folder-a".to_owned(),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response, StatusCode::NO_CONTENT);

        let remaining = state.mounts.lock().await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].folder_id, "folder-b");
        drop(remaining);

        let conn = state.db.lock().await;
        let persisted = mount_store::list_mounts(&conn).unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].folder_id, "folder-b");
    }

    #[tokio::test]
    async fn delete_mount_unknown_folder_returns_404() {
        let state = test_state(vec![test_mount("/sync-a", "folder-a")]);

        let result = delete_mount_route(
            axum::extract::State(state),
            axum::Json(UnmountRequest {
                folder_id: "unknown-folder".to_owned(),
            }),
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DaemonError::NotFound(_)));
    }
}
