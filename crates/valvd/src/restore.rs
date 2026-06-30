use std::path::{Component, Path, PathBuf};

use axum::{extract::State, http::StatusCode, Json};
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

use crate::{DaemonState, ErrorResponse, MountState};

type HandlerError = (StatusCode, Json<ErrorResponse>);

pub(crate) async fn post_versions(
    State(state): State<DaemonState>,
    Json(req): Json<VersionsRequest>,
) -> Result<Json<VersionsResponse>, HandlerError> {
    let resolved = resolve_local_path(&state, &req.local_path).await?;
    let token = resolved.mount.effective_token(&state.config).to_owned();
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
        .await
        .map_err(internal_server_error)?;
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
) -> Result<Json<RestoreResponse>, HandlerError> {
    let resolved = resolve_local_path(&state, &req.local_path).await?;
    let based_on_seq = {
        let conn = state.db.lock().await;
        let Some(node) =
            nodes::get_node(&conn, &resolved.node_id).map_err(internal_server_error)?
        else {
            return Err(not_found("path not found in local mirror"));
        };
        node.server_seq
    };
    let token = resolved.mount.effective_token(&state.config).to_owned();
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
        .await
        .map_err(internal_server_error)?;
    let response = parse_backend_json::<SubmitOpResponse>(response).await?;

    Ok(Json(RestoreResponse {
        result: response.result_str().to_owned(),
    }))
}

#[derive(Debug)]
struct ResolvedPath {
    mount: MountState,
    folder_id: String,
    node_id: String,
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
) -> Result<ResolvedPath, HandlerError> {
    let local_path = normalize_path(local_path);
    let (mount, relative_path) = {
        let mounts = state.mounts.lock().await;
        mounts
            .iter()
            .filter_map(|mount| {
                let mount_path = normalize_path(&mount.path);
                local_path.strip_prefix(&mount_path).ok().map(|relative| {
                    (
                        mount.clone(),
                        relative.to_path_buf(),
                        mount_path.components().count(),
                    )
                })
            })
            .max_by_key(|(_, _, component_count)| *component_count)
            .map(|(mount, relative, _)| (mount, relative))
            .ok_or_else(|| not_found("path is not under any mounted folder"))?
    };

    let conn = state.db.lock().await;
    let mut current = match mount.scope_node_id.as_deref() {
        Some(scope_node_id) => {
            nodes::get_node(&conn, scope_node_id).map_err(internal_server_error)?
        }
        None => nodes::get_root_node(&conn, &mount.folder_id).map_err(internal_server_error)?,
    }
    .ok_or_else(|| not_found("path not found in local mirror"))?;

    for component in relative_path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name
            .to_str()
            .ok_or_else(|| not_found("path not found in local mirror"))?;
        current = nodes::get_node_by_parent_and_name(
            &conn,
            &mount.folder_id,
            Some(&current.node_id),
            name,
        )
        .map_err(internal_server_error)?
        .ok_or_else(|| not_found("path not found in local mirror"))?;
    }

    Ok(ResolvedPath {
        folder_id: mount.folder_id.clone(),
        node_id: current.node_id,
        mount,
    })
}

fn normalize_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf())
}

async fn parse_backend_json<T: for<'de> Deserialize<'de>>(
    response: Response,
) -> Result<T, HandlerError> {
    if response.status().is_success() {
        return response.json::<T>().await.map_err(internal_server_error);
    }
    Err(internal_server_error(backend_error_message(response).await))
}

async fn backend_error_message(response: Response) -> String {
    let status = response.status();
    match response.text().await {
        Ok(text) if !text.trim().is_empty() => {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => value
                    .get("error")
                    .and_then(|error| error.as_str())
                    .map(str::to_owned)
                    .unwrap_or(text),
                Err(_) => text,
            }
        }
        _ => format!("backend returned {status}"),
    }
}

fn not_found(error: impl Into<String>) -> HandlerError {
    (StatusCode::NOT_FOUND, Json(ErrorResponse::new(error)))
}

fn internal_server_error(error: impl std::fmt::Display) -> HandlerError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(error.to_string())),
    )
}
