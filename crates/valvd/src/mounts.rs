use std::path::Path;

use anyhow::{anyhow, Result};
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use valv_sync::{
    persistence::mounts as mount_store,
    protocol::ipc::{MountRequest, MountResponse},
    sync_engine::delta_pull::tree_resync,
};

use crate::{
    internal_error,
    tasks::{cancel_mount_tasks, materialize_mount_files, spawn_mount_tasks},
    DaemonState, ErrorResponse, MountState,
};

pub(crate) async fn post_mount(
    State(state): State<DaemonState>,
    Json(req): Json<MountRequest>,
) -> Result<Json<MountResponse>, (StatusCode, Json<ErrorResponse>)> {
    if req.folder_id.is_some() && req.grant_token.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "folder_id_and_grant_token_are_mutually_exclusive",
            )),
        ));
    }

    let resolved = resolve_mount(&state, &req).await.map_err(internal_error)?;
    let token = resolved
        .mount_token
        .as_deref()
        .unwrap_or(&state.config.device_token)
        .to_owned();
    {
        let mut conn = state.db.lock().await;
        mount_store::upsert_mount(
            &conn,
            &req.path,
            &resolved.folder_id,
            resolved.grant_id.as_deref(),
            resolved.scope_node_id.as_deref(),
            resolved.mount_token.as_deref(),
        )
        .map_err(internal_error)?;
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
            return Err(internal_error(err));
        }
    }
    let mount = MountState {
        path: req.path.clone(),
        folder_id: resolved.folder_id.clone(),
        grant_id: resolved.grant_id.clone(),
        scope_node_id: resolved.scope_node_id.clone(),
        mount_token: resolved.mount_token,
        syncing: false,
        pending_ops: 0,
        last_synced_at: None,
        error: None,
    };
    if let Err(err) = materialize_mount_files(&state, &mount).await {
        let conn = state.db.lock().await;
        let _ = mount_store::delete_mount(&conn, &req.path);
        return Err(internal_error(err));
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
    cancel_mount_tasks(&state).await;
    spawn_mount_tasks(&state).await;

    Ok(Json(MountResponse {
        folder_id: resolved.folder_id,
        grant_id: resolved.grant_id,
        scope_node_id: resolved.scope_node_id,
        path: req.path,
    }))
}

#[derive(Debug)]
struct ResolvedMount {
    folder_id: String,
    grant_id: Option<String>,
    scope_node_id: Option<String>,
    mount_token: Option<String>,
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
}

async fn resolve_mount(state: &DaemonState, req: &MountRequest) -> Result<ResolvedMount> {
    if let Some(grant_token) = &req.grant_token {
        let grants = state
            .client
            .get(format!(
                "{}/grants",
                valv_sync::api_base(&state.config.backend_url)
            ))
            .bearer_auth(grant_token)
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<GrantListEntry>>()
            .await?;
        let grant = grants
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("grant token has no accessible grants"))?;
        return Ok(ResolvedMount {
            folder_id: grant.folder_id,
            grant_id: Some(grant.grant_id),
            scope_node_id: Some(grant.scope_node_id),
            mount_token: Some(grant_token.clone()),
        });
    }

    if let Some(folder_id) = &req.folder_id {
        return Ok(ResolvedMount {
            folder_id: folder_id.clone(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
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
        .await?
        .error_for_status()?
        .json::<CreateFolderResponse>()
        .await?;
    Ok(ResolvedMount {
        folder_id: created.folder_id,
        grant_id: None,
        scope_node_id: None,
        mount_token: None,
    })
}
