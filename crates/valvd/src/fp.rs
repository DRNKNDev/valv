use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use anyhow::{anyhow, Result};
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use reqwest::header::{HeaderName, HeaderValue};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use valv_sync::{
    api_base,
    chunking::{chunk_file, Chunk},
    persistence::{chunks as chunk_store, mounts, nodes, versions, LocalNode},
    protocol::{
        http::{BatchOperation, BatchRequest, BatchRequestObject, BatchResponse},
        ipc::{
            FpAnchorResponse, FpChangesResponse, FpChunkDownload, FpContentResponse,
            FpDeleteRequest, FpEnumerateResponse, FpItem, FpMoveRequest, FpMoveResponse,
            FpShareRequest, FpShareResponse, FpUploadQueued, FpUploadRequest,
        },
        sync::{
            manifest_content_hash, ChunkRef, CreatePayload, DeletePayload, MovePayload,
            NewVersionPayload, NodeType, RenamePayload, SubmitOpRequest, SubmitOpResponse,
            PROTOCOL_HEADER, PROTOCOL_VERSION,
        },
    },
    sync_engine::{
        op_submit::parse_submit_op_response_body,
        update_required::{update_required_from_response, UpdateRequired},
    },
};

use crate::{
    error::{backend_response_or_error, DaemonError},
    tasks::mark_mount_update_required,
    DaemonState, MountState,
};

pub(crate) async fn fp_items(
    State(state): State<DaemonState>,
    Query(query): Query<FpItemsQuery>,
) -> Result<Json<FpEnumerateResponse>, DaemonError> {
    if query.parent.trim().is_empty() {
        return Err(DaemonError::BadRequest("parent is required".to_owned()));
    }
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref()).await?;
    let limit = query.limit.unwrap_or(200).min(200);
    let offset = query.offset.unwrap_or(0);
    let conn = state.db.lock().await;
    let parent = resolve_parent_id(&conn, &mount, &query.parent)?;
    let (nodes, total) =
        nodes::list_children(&conn, parent.as_deref(), &mount.folder_id, offset, limit)?;
    let items = nodes
        .iter()
        .map(|node| fp_item_from_node(&conn, node))
        .collect::<Result<Vec<_>>>()?;
    let synced_to_seq = mounts::get_cursor(&conn, &mount.folder_id)?;
    Ok(Json(FpEnumerateResponse {
        items,
        total,
        synced_to_seq,
        can_write: mount.can_write,
    }))
}

pub(crate) async fn fp_item(
    State(state): State<DaemonState>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<FpItem>, DaemonError> {
    let conn = state.db.lock().await;
    let Some(node) = nodes::get_node(&conn, &node_id)? else {
        return Err(DaemonError::NotFound("node_not_found".to_owned()));
    };
    Ok(Json(fp_item_from_node(&conn, &node)?))
}

pub(crate) async fn fp_anchor(
    State(state): State<DaemonState>,
    Query(query): Query<FpFolderQuery>,
) -> Result<Json<FpAnchorResponse>, DaemonError> {
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref()).await?;
    let conn = state.db.lock().await;
    Ok(Json(FpAnchorResponse {
        server_seq: mounts::get_cursor(&conn, &mount.folder_id)?,
        can_write: mount.can_write,
    }))
}

const FP_WATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub(crate) async fn fp_watch(
    State(state): State<DaemonState>,
    Query(query): Query<FpWatchQuery>,
) -> Result<Json<FpAnchorResponse>, DaemonError> {
    fp_watch_inner(state, query, FP_WATCH_TIMEOUT).await
}

async fn fp_watch_inner(
    state: DaemonState,
    query: FpWatchQuery,
    timeout: std::time::Duration,
) -> Result<Json<FpAnchorResponse>, DaemonError> {
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref()).await?;
    // Register as a waiter before checking the cursor: `Notify::notified()`'s
    // future, once created, captures any `notify_waiters()` call that happens
    // after this point even if we haven't started awaiting it yet - checking
    // the cursor first and only then calling `.notified()` would leave a race
    // where a cursor advance between the check and the call is silently missed.
    let notified = mount.cursor_notify.notified();
    let current_seq = {
        let conn = state.db.lock().await;
        mounts::get_cursor(&conn, &mount.folder_id)?
    };
    if current_seq <= query.since_seq {
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(timeout) => {}
        }
    }
    let server_seq = {
        let conn = state.db.lock().await;
        mounts::get_cursor(&conn, &mount.folder_id)?
    };
    Ok(Json(FpAnchorResponse {
        server_seq,
        can_write: mount.can_write,
    }))
}

pub(crate) async fn fp_changes(
    State(state): State<DaemonState>,
    Query(query): Query<FpChangesQuery>,
) -> Result<Json<FpChangesResponse>, DaemonError> {
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref()).await?;
    let conn = state.db.lock().await;
    let nodes = nodes::list_changed_since(&conn, &mount.folder_id, query.since_seq.unwrap_or(0))?;
    let pending_uploads = state.pending_uploads.lock().await.clone();
    let mut items = Vec::new();
    let mut newly_deferred = Vec::new();
    for node in &nodes {
        let item = fp_item_from_node(&conn, node)?;
        if item.deleted && pending_uploads.contains(&item.node_id) {
            newly_deferred.push(item.node_id);
        } else {
            items.push(item);
        }
    }
    let mut deferred_deletes = state.deferred_deletes.lock().await;
    if !newly_deferred.is_empty() {
        deferred_deletes
            .entry(mount.folder_id.clone())
            .or_default()
            .extend(newly_deferred);
    }
    let ready_deferred = deferred_deletes
        .get(&mount.folder_id)
        .map(|node_ids| {
            node_ids
                .iter()
                .filter(|node_id| !pending_uploads.contains(*node_id))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for node_id in &ready_deferred {
        if let Some(node) = nodes::get_node(&conn, node_id)? {
            items.push(fp_item_from_node(&conn, &node)?);
        }
    }
    if let Some(node_ids) = deferred_deletes.get_mut(&mount.folder_id) {
        for node_id in ready_deferred {
            node_ids.remove(&node_id);
        }
        if node_ids.is_empty() {
            deferred_deletes.remove(&mount.folder_id);
        }
    }
    let current_seq = mounts::get_cursor(&conn, &mount.folder_id)?;
    Ok(Json(FpChangesResponse {
        items,
        current_seq,
        more_coming: false,
    }))
}

pub(crate) async fn fp_content(
    State(state): State<DaemonState>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<FpContentResponse>, DaemonError> {
    let (mount, version) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &node_id)? else {
            return Err(DaemonError::NotFound("node_not_found".to_owned()));
        };
        let Some(version_id) = node.current_version_id.as_deref() else {
            return Err(DaemonError::NotFound("version_not_found".to_owned()));
        };
        let Some(version) = versions::get_version(&conn, version_id)? else {
            return Err(DaemonError::NotFound("version_not_found".to_owned()));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id).await?;
        (mount, version)
    };
    let manifest = serde_json::from_str::<Vec<ChunkRef>>(&version.manifest_json)?;
    let objects = manifest
        .iter()
        .map(|chunk| BatchRequestObject {
            oid: chunk.chunk_hash.clone(),
            size: chunk.length,
        })
        .collect::<Vec<_>>();
    let token = mount.effective_token(&state.config).to_owned();
    let batch = state
        .client
        .post(format!(
            "{}/objects/batch",
            valv_sync::api_base(&state.config.backend_url)
        ))
        .bearer_auth(token)
        .json(&BatchRequest::new(BatchOperation::Download, objects))
        .send()
        .await?;
    let batch = backend_response_or_error(batch)
        .await?
        .json::<BatchResponse>()
        .await?;
    let chunks = manifest
        .iter()
        .map(|chunk| {
            let object = batch
                .objects
                .iter()
                .find(|object| object.oid == chunk.chunk_hash)
                .ok_or_else(|| anyhow!("missing batch object for {}", chunk.chunk_hash))?;
            let action = object
                .actions
                .as_ref()
                .and_then(|actions| actions.download.as_ref())
                .ok_or_else(|| anyhow!("missing download action for {}", chunk.chunk_hash))?;
            Ok(FpChunkDownload {
                chunk_hash: chunk.chunk_hash.clone(),
                offset: chunk.offset,
                length: chunk.length,
                url: action.href.clone(),
                expires_in: action.expires_in.unwrap_or(0),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Json(FpContentResponse {
        version_id: version.version_id,
        size_bytes: version.size_bytes,
        chunks,
    }))
}

pub(crate) async fn fp_upload(
    State(state): State<DaemonState>,
    Json(req): Json<FpUploadRequest>,
) -> Result<(StatusCode, Json<FpUploadQueued>), DaemonError> {
    validate_upload_request(&req)?;
    let node_id = req
        .node_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let context = resolve_upload_context(&state, &req, &node_id).await?;
    state.pending_uploads.lock().await.insert(node_id.clone());
    tokio::spawn(upload_job_with_context(
        state,
        req,
        node_id.clone(),
        context,
    ));
    Ok((
        StatusCode::ACCEPTED,
        Json(FpUploadQueued {
            queued: true,
            node_id,
        }),
    ))
}

pub(crate) async fn fp_delete(
    State(state): State<DaemonState>,
    Json(req): Json<FpDeleteRequest>,
) -> Result<StatusCode, DaemonError> {
    if req.node_id.trim().is_empty() {
        return Err(DaemonError::BadRequest("node_id is required".to_owned()));
    }
    let (mount, token) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &req.node_id)? else {
            return Err(DaemonError::NotFound("node_not_found".to_owned()));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id).await?;
        (
            mount.clone(),
            mount.effective_token(&state.config).to_owned(),
        )
    };
    let response = submit_op_daemon_for_mount(
        &state,
        &mount,
        &token,
        &SubmitOpRequest::Delete {
            node_id: req.node_id,
            based_on_seq: req.based_on_seq,
            payload: DeletePayload {},
        },
    )
    .await?;
    match response {
        SubmitOpResponse::Applied { .. } => Ok(StatusCode::NO_CONTENT),
        SubmitOpResponse::Superseded { .. } => Err(DaemonError::Conflict(json!({
            "error": "superseded",
            "message": "delete was superseded; re-sync before retrying"
        }))),
        SubmitOpResponse::ConflictCopy { .. } => Err(DaemonError::Conflict(json!({
            "error": "conflict_copy",
            "message": "delete conflicted with a concurrent write; re-sync before retrying"
        }))),
        SubmitOpResponse::Conflict { .. } => Err(DaemonError::Conflict(json!({
            "error": "conflict",
            "message": "delete conflicted with a concurrent write; re-sync before retrying"
        }))),
    }
}

pub(crate) async fn fp_move(
    State(state): State<DaemonState>,
    Json(req): Json<FpMoveRequest>,
) -> Result<Json<FpMoveResponse>, DaemonError> {
    validate_move_request(&req)?;
    let (mount, token, mut updated_node) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &req.node_id)? else {
            return Err(DaemonError::NotFound("node_not_found".to_owned()));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id).await?;
        (
            mount.clone(),
            mount.effective_token(&state.config).to_owned(),
            node,
        )
    };

    let mut based_on_seq = req.based_on_seq;
    let mut applied_seq = None;

    if let Some(new_name) = req.new_name.as_deref() {
        let response = submit_op_daemon_for_mount(
            &state,
            &mount,
            &token,
            &SubmitOpRequest::Rename {
                node_id: req.node_id.clone(),
                based_on_seq,
                payload: RenamePayload {
                    new_name: new_name.to_owned(),
                },
            },
        )
        .await?;
        based_on_seq = applied_server_seq(response)?;
        updated_node.name = new_name.to_owned();
        updated_node.server_seq = based_on_seq;
        {
            let conn = state.db.lock().await;
            nodes::upsert_node(&conn, &updated_node)?;
        }
        applied_seq = Some(based_on_seq);
    }

    if let Some(new_parent_id) = req.new_parent_id.as_deref() {
        {
            let conn = state.db.lock().await;
            if let Some(parent) = nodes::get_node(&conn, new_parent_id)? {
                if parent.folder_id != updated_node.folder_id {
                    return Err(DaemonError::Conflict(json!({
                        "error": "cross_folder_move_rejected"
                    })));
                }
            }
        }
        let response = submit_op_daemon_for_mount(
            &state,
            &mount,
            &token,
            &SubmitOpRequest::Move {
                node_id: req.node_id.clone(),
                based_on_seq,
                payload: MovePayload {
                    new_parent_id: new_parent_id.to_owned(),
                },
            },
        )
        .await?;
        based_on_seq = applied_server_seq(response)?;
        updated_node.parent_id = Some(new_parent_id.to_owned());
        updated_node.server_seq = based_on_seq;
        {
            let conn = state.db.lock().await;
            nodes::upsert_node(&conn, &updated_node)?;
        }
        applied_seq = Some(based_on_seq);
    }

    let server_seq = applied_seq.expect("validated request contains at least one change");
    Ok(Json(FpMoveResponse {
        node_id: req.node_id,
        server_seq,
    }))
}

pub(crate) async fn fp_share(
    State(state): State<DaemonState>,
    Json(req): Json<FpShareRequest>,
) -> Result<Json<FpShareResponse>, DaemonError> {
    if req.node_id.trim().is_empty() {
        return Err(DaemonError::BadRequest("node_id is required".to_owned()));
    }
    if req.invited_email.trim().is_empty() {
        return Err(DaemonError::BadRequest(
            "invited_email is required".to_owned(),
        ));
    }
    let (folder_id, token) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &req.node_id)? else {
            return Err(DaemonError::NotFound("node_not_found".to_owned()));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id).await?;
        (
            node.folder_id,
            mount.effective_token(&state.config).to_owned(),
        )
    };
    let invite = state
        .client
        .post(format!(
            "{}/folders/{}/invites",
            valv_sync::api_base(&state.config.backend_url),
            folder_id
        ))
        .bearer_auth(&token)
        .json(&InviteCreateRequest {
            invited_email: req.invited_email,
            scope_node_id: req.node_id,
            can_write: req.can_write,
        })
        .send()
        .await?;
    let invite = backend_response_or_error(invite)
        .await?
        .json::<InviteCreateResponse>()
        .await?;
    Ok(Json(FpShareResponse {
        invite_url: format!(
            "{}/invites/{}/accept",
            valv_sync::api_base(&state.config.backend_url),
            invite.invite_token
        ),
    }))
}

#[derive(Debug, serde::Serialize)]
struct InviteCreateRequest {
    invited_email: String,
    scope_node_id: String,
    can_write: bool,
}

#[derive(Debug, Deserialize)]
struct InviteCreateResponse {
    invite_token: String,
}

fn validate_upload_request(req: &FpUploadRequest) -> Result<(), DaemonError> {
    if req.node_id.as_deref().is_some_and(str::is_empty) {
        return Err(DaemonError::BadRequest(
            "node_id cannot be empty".to_owned(),
        ));
    }
    if req.parent_id.trim().is_empty() {
        return Err(DaemonError::BadRequest("parent_id is required".to_owned()));
    }
    if req.name.trim().is_empty() {
        return Err(DaemonError::BadRequest("name is required".to_owned()));
    }
    if req.file_path.trim().is_empty() {
        return Err(DaemonError::BadRequest("file_path is required".to_owned()));
    }
    Ok(())
}

fn validate_move_request(req: &FpMoveRequest) -> Result<(), DaemonError> {
    if req.node_id.trim().is_empty() {
        return Err(DaemonError::BadRequest("node_id is required".to_owned()));
    }
    if req.new_name.is_none() && req.new_parent_id.is_none() {
        return Err(DaemonError::BadRequest(
            "new_name or new_parent_id is required".to_owned(),
        ));
    }
    if req.new_name.as_deref().is_some_and(str::is_empty) {
        return Err(DaemonError::BadRequest(
            "new_name cannot be empty".to_owned(),
        ));
    }
    if req.new_parent_id.as_deref().is_some_and(str::is_empty) {
        return Err(DaemonError::BadRequest(
            "new_parent_id cannot be empty".to_owned(),
        ));
    }
    Ok(())
}

fn applied_server_seq(response: SubmitOpResponse) -> Result<i64, DaemonError> {
    match response {
        SubmitOpResponse::Applied { server_seq, .. } => Ok(server_seq),
        SubmitOpResponse::Superseded { current_seq } => Err(DaemonError::Conflict(json!({
            "error": "superseded",
            "current_seq": current_seq
        }))),
        SubmitOpResponse::ConflictCopy { server_seq, .. } => Err(DaemonError::Conflict(json!({
            "error": "conflict_copy",
            "server_seq": server_seq
        }))),
        SubmitOpResponse::Conflict {
            current_server_seq, ..
        } => Err(DaemonError::Conflict(json!({
            "error": "conflict",
            "current_server_seq": current_server_seq
        }))),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct FpItemsQuery {
    parent: String,
    folder_id: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FpFolderQuery {
    folder_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FpChangesQuery {
    folder_id: Option<String>,
    #[serde(alias = "since")]
    since_seq: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FpWatchQuery {
    folder_id: Option<String>,
    since_seq: i64,
}

fn resolve_parent_id(
    conn: &Connection,
    mount: &MountState,
    parent: &str,
) -> Result<Option<String>> {
    if parent != "root" {
        return Ok(Some(parent.to_owned()));
    }
    if let Some(scope_node_id) = mount.scope_node_id.as_deref() {
        return Ok(Some(scope_node_id.to_owned()));
    }
    Ok(nodes::get_root_node(conn, &mount.folder_id)?.map(|node| node.node_id))
}

async fn resolve_mount_for_query(
    state: &DaemonState,
    folder_id: Option<&str>,
) -> Result<MountState, DaemonError> {
    let mounts = state.mounts.lock().await;
    if let Some(folder_id) = folder_id {
        if folder_id.is_empty() {
            return Err(DaemonError::BadRequest(
                "folder_id cannot be empty".to_owned(),
            ));
        }
        mounts
            .iter()
            .find(|mount| mount.folder_id == folder_id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound("mount_not_found".to_owned()))
    } else {
        match mounts.as_slice() {
            [mount] => Ok(mount.clone()),
            [] => Err(DaemonError::NotFound("mount_not_found".to_owned())),
            _ => Err(DaemonError::BadRequest(
                "folder_id is required when multiple folders are mounted".to_owned(),
            )),
        }
    }
}

async fn resolve_mount_for_folder_id(
    state: &DaemonState,
    folder_id: &str,
) -> Result<MountState, DaemonError> {
    state
        .mounts
        .lock()
        .await
        .iter()
        .find(|mount| mount.folder_id == folder_id)
        .cloned()
        .ok_or_else(|| DaemonError::NotFound("mount_not_found".to_owned()))
}

fn fp_item_from_node(conn: &Connection, node: &LocalNode) -> Result<FpItem> {
    let (content_hash, size_bytes) = match node.current_version_id.as_deref() {
        Some(version_id) => match versions::get_version(conn, version_id)? {
            Some(version) => (Some(version.content_hash), Some(version.size_bytes)),
            None => (None, None),
        },
        None => (None, None),
    };
    Ok(FpItem {
        node_id: node.node_id.clone(),
        parent_id: node.parent_id.clone(),
        folder_id: node.folder_id.clone(),
        name: node.name.clone(),
        node_type: node.node_type.clone(),
        version_id: node.current_version_id.clone(),
        content_hash,
        size_bytes,
        server_seq: node.server_seq,
        deleted: node.deleted_at.is_some(),
    })
}

#[cfg(test)]
async fn upload_job(state: DaemonState, req: FpUploadRequest, node_id: String) {
    let context = match resolve_upload_context(&state, &req, &node_id).await {
        Ok(context) => context,
        Err(error) => {
            tracing::error!(
                node_id = %node_id,
                error = %error,
                "file provider upload failed before resolving mount"
            );
            return;
        }
    };
    upload_job_with_context(state, req, node_id, context).await;
}

async fn upload_job_with_context(
    state: DaemonState,
    req: FpUploadRequest,
    node_id: String,
    context: UploadContext,
) {
    let folder_id = context.folder_id.clone();
    let result = upload_job_inner(&state, req, node_id.clone(), &context).await;
    state.pending_uploads.lock().await.remove(&node_id);
    if let Err(error) = result {
        let message = upload_failure_message(&error);
        set_upload_mount_error(&state, &context, Some(message.clone())).await;
        tracing::error!(
            folder_id = %folder_id,
            node_id = %node_id,
            error = %error,
            status_error = %message,
            "file provider upload failed"
        );
    }
}

struct UploadContext {
    folder_id: String,
    token: String,
    create_first: bool,
    based_on_seq: i64,
    parent_id: String,
    initial_error: Option<String>,
    sync_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    cursor_notify: std::sync::Arc<tokio::sync::Notify>,
    update_required: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

async fn resolve_upload_context(
    state: &DaemonState,
    req: &FpUploadRequest,
    node_id: &str,
) -> Result<UploadContext, DaemonError> {
    let conn = state.db.lock().await;
    let (mount, parent_id, parent) = if req.parent_id == "root" {
        let mount = resolve_mount_for_query(state, None).await?;
        let parent_id = resolve_parent_id(&conn, &mount, &req.parent_id)?.ok_or_else(|| {
            DaemonError::NotFound(format!("parent node not found: {}", req.parent_id))
        })?;
        let parent = nodes::get_node(&conn, &parent_id)?
            .ok_or_else(|| DaemonError::NotFound(format!("parent node not found: {parent_id}")))?;
        (mount, parent_id, parent)
    } else {
        let parent = nodes::get_node(&conn, &req.parent_id)?.ok_or_else(|| {
            DaemonError::NotFound(format!("parent node not found: {}", req.parent_id))
        })?;
        let mount = resolve_mount_for_folder_id(state, &parent.folder_id).await?;
        (mount, req.parent_id.clone(), parent)
    };
    ensure_mount_not_update_required(&mount)?;
    let existing = nodes::get_node(&conn, node_id)?.or(nodes::get_node_by_parent_and_name(
        &conn,
        &parent.folder_id,
        Some(&parent_id),
        &req.name,
    )?);
    let create_first = existing.is_none();
    let based_on_seq = existing
        .as_ref()
        .map(|node| node.server_seq)
        .or(req.based_on_seq)
        .unwrap_or(parent.server_seq);
    Ok(UploadContext {
        folder_id: parent.folder_id,
        token: mount.effective_token(&state.config).to_owned(),
        create_first,
        based_on_seq,
        parent_id,
        initial_error: mount.error.clone(),
        sync_lock: mount.sync_lock.clone(),
        cursor_notify: mount.cursor_notify.clone(),
        update_required: mount.update_required_flag.clone(),
    })
}

async fn upload_job_inner(
    state: &DaemonState,
    req: FpUploadRequest,
    node_id: String,
    context: &UploadContext,
) -> Result<(), DaemonError> {
    let based_on_seq = if context.create_first {
        match submit_op_daemon_for_upload_context(
            state,
            context,
            &context.token,
            &SubmitOpRequest::Create {
                payload: CreatePayload {
                    node_id: node_id.clone(),
                    parent_id: context.parent_id.clone(),
                    name: req.name.clone(),
                    node_type: NodeType::File,
                },
            },
        )
        .await?
        {
            SubmitOpResponse::Applied { server_seq, .. } => server_seq,
            SubmitOpResponse::Superseded { .. } => {
                return Err(DaemonError::Conflict(json!({
                    "error": "superseded",
                    "message": format!("create op was superseded for {node_id}")
                })));
            }
            SubmitOpResponse::ConflictCopy { .. } | SubmitOpResponse::Conflict { .. } => {
                context.based_on_seq
            }
        }
    } else {
        context.based_on_seq
    };

    let path = Path::new(&req.file_path);
    let chunks = chunk_file(path)?;
    let pending = {
        let conn = state.db.lock().await;
        chunks
            .iter()
            .filter_map(|chunk| match chunk_store::is_uploaded(&conn, &chunk.hash) {
                Ok(true) => None,
                Ok(false) => Some(Ok(chunk.clone())),
                Err(err) => Some(Err(err)),
            })
            .collect::<Result<Vec<_>>>()?
    };
    upload_pending_chunks(
        &state.client,
        &state.config.backend_url,
        &context.token,
        &pending,
    )
    .await?;
    {
        let conn = state.db.lock().await;
        for chunk in &pending {
            chunk_store::mark_uploaded(&conn, &chunk.hash, chunk.length)?;
        }
    }

    let manifest = chunks
        .iter()
        .map(|chunk| ChunkRef {
            chunk_hash: chunk.hash.clone(),
            offset: chunk.offset,
            length: chunk.length,
        })
        .collect::<Vec<_>>();
    let response = submit_op_daemon_for_upload_context(
        state,
        context,
        &context.token,
        &SubmitOpRequest::NewVersion {
            node_id,
            based_on_seq,
            payload: NewVersionPayload {
                version_id: Uuid::new_v4().to_string(),
                content_hash: manifest_content_hash(&manifest),
                size_bytes: chunks.iter().map(|chunk| chunk.length).sum(),
                manifest,
                force_conflict_copy: false,
            },
        },
    )
    .await?;
    if matches!(response, SubmitOpResponse::ConflictCopy { .. }) {
        materialize_conflict_copy_name(path, &state.config.device_name)?;
    }
    set_upload_mount_error(state, &context, None).await;
    context.cursor_notify.notify_waiters();
    Ok(())
}

fn ensure_mount_not_update_required(mount: &MountState) -> Result<(), DaemonError> {
    if mount.update_required || mount.update_required_flag.load(Ordering::Acquire) {
        return Err(DaemonError::UpdateRequired(
            UpdateRequired::mount_already_halted(),
        ));
    }
    Ok(())
}

fn ensure_upload_context_not_update_required(context: &UploadContext) -> Result<(), DaemonError> {
    if context.update_required.load(Ordering::Acquire) {
        return Err(DaemonError::UpdateRequired(
            UpdateRequired::mount_already_halted(),
        ));
    }
    Ok(())
}

async fn submit_op_daemon_for_mount(
    state: &DaemonState,
    mount: &MountState,
    token: &str,
    req: &SubmitOpRequest,
) -> Result<SubmitOpResponse, DaemonError> {
    ensure_mount_not_update_required(mount)?;
    let result = submit_op_daemon(
        &state.client,
        &state.config.backend_url,
        token,
        &mount.folder_id,
        req,
    )
    .await;
    if matches!(result, Err(DaemonError::UpdateRequired(_))) {
        mark_mount_update_required(state, &mount.folder_id).await;
    }
    result
}

async fn submit_op_daemon_for_upload_context(
    state: &DaemonState,
    context: &UploadContext,
    token: &str,
    req: &SubmitOpRequest,
) -> Result<SubmitOpResponse, DaemonError> {
    ensure_upload_context_not_update_required(context)?;
    let result = submit_op_daemon(
        &state.client,
        &state.config.backend_url,
        token,
        &context.folder_id,
        req,
    )
    .await;
    if matches!(result, Err(DaemonError::UpdateRequired(_))) {
        mark_mount_update_required(state, &context.folder_id).await;
    }
    result
}

async fn set_upload_mount_error(
    state: &DaemonState,
    context: &UploadContext,
    error: Option<String>,
) {
    let _sync_guard = context.sync_lock.lock().await;
    let mut mounts = state.mounts.lock().await;
    if let Some(mount) = mounts
        .iter_mut()
        .find(|mount| mount.folder_id == context.folder_id)
    {
        match error {
            Some(error) => mount.error = Some(error),
            None if mount.error == context.initial_error => mount.error = None,
            None => {}
        }
    }
}

async fn upload_pending_chunks(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    chunks: &[Chunk],
) -> Result<(), DaemonError> {
    if chunks.is_empty() {
        return Ok(());
    }
    let objects = chunks
        .iter()
        .map(|chunk| BatchRequestObject {
            oid: chunk.hash.clone(),
            size: chunk.length,
        })
        .collect::<Vec<_>>();
    let batch = client
        .post(format!(
            "{}/objects/batch",
            valv_sync::api_base(backend_url)
        ))
        .bearer_auth(token)
        .json(&BatchRequest::new(BatchOperation::Upload, objects))
        .send()
        .await?;
    let batch = backend_response_or_error(batch)
        .await?
        .json::<BatchResponse>()
        .await?;
    for object in batch.objects {
        if let Some(error) = object.error {
            return Err(anyhow!("batch upload error for {}: {}", object.oid, error.message).into());
        }
        let Some(action) = object.actions.and_then(|actions| actions.upload) else {
            continue;
        };
        let chunk = chunks
            .iter()
            .find(|chunk| chunk.hash == object.oid)
            .ok_or_else(|| anyhow!("batch response referenced unknown oid {}", object.oid))?;
        let mut request = client.put(&action.href).body(chunk.data.clone());
        for (name, value) in action.header.unwrap_or_default() {
            request = request.header(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            );
        }
        backend_response_or_error(request.send().await?).await?;
    }
    Ok(())
}

async fn submit_op_daemon(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    folder_id: &str,
    req: &SubmitOpRequest,
) -> Result<SubmitOpResponse, DaemonError> {
    let response = client
        .post(format!(
            "{}/folders/{}/ops",
            api_base(backend_url),
            folder_id
        ))
        .bearer_auth(token)
        .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
        .json(req)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::UPGRADE_REQUIRED {
        return Err(update_required_from_response(response).await.into());
    }
    let body = backend_response_or_error(response)
        .await?
        .json::<Value>()
        .await?;
    Ok(parse_submit_op_response_body(body)?)
}

fn upload_failure_message(error: &DaemonError) -> String {
    if let DaemonError::Backend { body, .. } = error {
        match body.get("error").and_then(|error| error.as_str()) {
            Some("subscription_inactive") => {
                return "Sync paused: subscription inactive".to_owned();
            }
            Some("over_quota") => return "Sync paused: storage quota exceeded".to_owned(),
            _ => {}
        }
    }
    error.to_string()
}

fn materialize_conflict_copy_name(path: &Path, device_name: &str) -> Result<()> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("path has no valid file name: {}", path.display()))?;
    let conflict_name = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| anyhow!("path has no valid file stem: {}", path.display()))?;
            format!("{stem} (conflicted copy, {device_name}, {date}).{ext}")
        }
        None => format!("{file_name} (conflicted copy, {device_name}, {date})"),
    };
    fs::copy(
        path,
        disambiguate_conflict_path(path.with_file_name(conflict_name))?,
    )?;
    Ok(())
}

fn disambiguate_conflict_path(desired: PathBuf) -> Result<PathBuf> {
    if !desired.exists() {
        return Ok(desired);
    }
    let parent = desired.parent().map(Path::to_path_buf);
    let stem = desired
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("path has no valid file stem: {}", desired.display()))?;
    let extension = desired.extension().and_then(|ext| ext.to_str());
    for counter in 2..=20 {
        let file_name = match extension {
            Some(ext) => format!("{stem} ({counter}).{ext}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = parent.as_ref().map_or_else(
            || PathBuf::from(&file_name),
            |parent| parent.join(&file_name),
        );
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "conflict copy path exhausted disambiguation attempts for {}",
        desired.display()
    ))
}

#[cfg(test)]
mod fp_watch_tests {
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicBool, Arc},
        time::{Duration, Instant},
    };

    use tokio::sync::{Mutex, Notify};

    use crate::config::DaemonConfig;

    use super::*;

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
            cursor_notify: Arc::new(Notify::new()),
        }
    }

    fn test_state(mount: MountState) -> DaemonState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        mounts::upsert_mount(
            &conn,
            &mount.path,
            &mount.folder_id,
            None,
            None,
            None,
            mount.can_write,
        )
        .unwrap();
        DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(vec![mount])),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            update_status: Arc::new(Mutex::new(Default::default())),
            backend_health: Arc::new(crate::BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(std::collections::HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url: "http://127.0.0.1:1".to_owned(),
                device_id: "device-1".to_owned(),
                device_token: "token".to_owned(),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn returns_immediately_when_already_stale() {
        let state = test_state(test_mount("/sync", "folder-1"));
        {
            let conn = state.db.lock().await;
            mounts::set_cursor(&conn, "folder-1", 15).unwrap();
        }

        let started = Instant::now();
        let response = fp_watch(
            State(state),
            Query(FpWatchQuery {
                folder_id: Some("folder-1".into()),
                since_seq: 10,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 15);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn fp_conflict_copy_name_gets_counter_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        fs::write(&path, b"new").unwrap();
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let base = path.with_file_name(format!("report (conflicted copy, Test Device, {date}).md"));
        fs::write(&base, b"first").unwrap();

        materialize_conflict_copy_name(&path, "Test Device").unwrap();
        let second = path.with_file_name(format!(
            "report (conflicted copy, Test Device, {date}) (2).md"
        ));

        assert_eq!(fs::read(base).unwrap(), b"first");
        assert_eq!(fs::read(second).unwrap(), b"new");
    }

    #[tokio::test]
    async fn fp_changes_withholds_delete_for_pending_upload_and_keeps_current_seq() {
        let state = state_with_deleted_node().await;
        state.pending_uploads.lock().await.insert("n1".to_owned());

        let response = fp_changes(
            State(state.clone()),
            Query(FpChangesQuery {
                folder_id: Some("folder-1".into()),
                since_seq: Some(0),
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.items.is_empty());
        assert_eq!(response.current_seq, 7);
        assert!(state
            .deferred_deletes
            .lock()
            .await
            .get("folder-1")
            .is_some_and(|node_ids| node_ids.contains("n1")));
    }

    #[tokio::test]
    async fn fp_changes_redelivers_deferred_delete_after_upload_clears() {
        let state = state_with_deleted_node().await;
        state.pending_uploads.lock().await.insert("n1".to_owned());
        let _ = fp_changes(
            State(state.clone()),
            Query(FpChangesQuery {
                folder_id: Some("folder-1".into()),
                since_seq: Some(0),
            }),
        )
        .await
        .unwrap();
        state.pending_uploads.lock().await.remove("n1");

        let response = fp_changes(
            State(state.clone()),
            Query(FpChangesQuery {
                folder_id: Some("folder-1".into()),
                since_seq: Some(100),
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].node_id, "n1");
        assert!(response.items[0].deleted);
        assert!(!state.deferred_deletes.lock().await.contains_key("folder-1"));
    }

    async fn state_with_deleted_node() -> DaemonState {
        let state = test_state(test_mount("/sync", "folder-1"));
        {
            let conn = state.db.lock().await;
            mounts::set_cursor(&conn, "folder-1", 7).unwrap();
            nodes::upsert_node(
                &conn,
                &LocalNode {
                    node_id: "n1".into(),
                    folder_id: "folder-1".into(),
                    parent_id: None,
                    name: "deleted.txt".into(),
                    node_type: "file".into(),
                    current_version_id: None,
                    server_seq: 7,
                    deleted_at: Some("2026-07-08T00:00:00Z".into()),
                },
            )
            .unwrap();
        }
        state
    }

    #[tokio::test]
    async fn wakes_on_notify_instead_of_waiting_out_the_timeout() {
        let state = test_state(test_mount("/sync", "folder-1"));
        {
            let conn = state.db.lock().await;
            mounts::set_cursor(&conn, "folder-1", 10).unwrap();
        }
        let notify = state.mounts.lock().await[0].cursor_notify.clone();
        let db = state.db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            {
                let conn = db.lock().await;
                mounts::set_cursor(&conn, "folder-1", 11).unwrap();
            }
            notify.notify_waiters();
        });

        let started = Instant::now();
        let response = fp_watch(
            State(state),
            Query(FpWatchQuery {
                folder_id: Some("folder-1".into()),
                since_seq: 10,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 11);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn times_out_with_unchanged_seq() {
        let state = test_state(test_mount("/sync", "folder-1"));
        {
            let conn = state.db.lock().await;
            mounts::set_cursor(&conn, "folder-1", 15).unwrap();
        }

        let response = fp_watch_inner(
            state,
            FpWatchQuery {
                folder_id: Some("folder-1".into()),
                since_seq: 15,
            },
            Duration::from_millis(50),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 15);
    }

    #[tokio::test]
    async fn unknown_folder_id_returns_404() {
        let state = test_state(test_mount("/sync", "folder-1"));

        let error = fp_watch(
            State(state),
            Query(FpWatchQuery {
                folder_id: Some("unknown-folder".into()),
                since_seq: 0,
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DaemonError::NotFound(_)));
    }
}

#[cfg(test)]
mod fp_error_tests {
    use std::{
        collections::HashMap,
        fs,
        sync::{atomic::AtomicBool, Arc},
        time::Duration,
    };

    use axum::extract::Query;
    use axum::{body, extract::State, response::IntoResponse, Json};
    use serde_json::Value;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{Mutex, Notify},
    };
    use valv_sync::{
        persistence::{mounts as mount_store, nodes as node_store, LocalNode},
        protocol::ipc::{FpDeleteRequest, FpMoveRequest, FpUploadRequest},
    };

    use crate::config::DaemonConfig;

    use super::*;

    async fn backend_url_with_response(status_line: &str, body: &'static str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status_line = status_line.to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer).await.unwrap();
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn backend_url_with_response_and_request(
        status_line: &str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status_line = status_line.to_owned();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let n = stream.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (format!("http://{addr}"), server)
    }

    async fn backend_url_recording_responses(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        tokio::spawn(async move {
            for (status_line, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request_body = read_http_json_body(&mut stream).await;
                captured.lock().await.push(request_body);
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{addr}"), requests)
    }

    async fn read_http_json_body(stream: &mut tokio::net::TcpStream) -> Value {
        let mut buffer = Vec::new();
        let mut chunk = [0; 1024];
        let header_end;
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "connection closed before request headers");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = pos + 4;
                break;
            }
        }
        let headers = std::str::from_utf8(&buffer[..header_end]).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .or_else(|| line.strip_prefix("Content-Length:"))
            })
            .map(str::trim)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "connection closed before request body");
            buffer.extend_from_slice(&chunk[..read]);
        }
        serde_json::from_slice(&buffer[header_end..header_end + content_length]).unwrap()
    }

    async fn response_json(error: DaemonError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        (status, body)
    }

    async fn daemon_status(state: DaemonState) -> valv_sync::protocol::ipc::DaemonStatus {
        let response = crate::control::get_status(State(state))
            .await
            .unwrap()
            .into_response();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn node(node_id: &str, parent_id: Option<&str>, name: &str) -> LocalNode {
        LocalNode {
            node_id: node_id.to_owned(),
            folder_id: "folder-1".to_owned(),
            parent_id: parent_id.map(str::to_owned),
            name: name.to_owned(),
            node_type: "file".into(),
            current_version_id: None,
            server_seq: 7,
            deleted_at: None,
        }
    }

    fn folder_node(node_id: &str, parent_id: Option<&str>, name: &str) -> LocalNode {
        LocalNode {
            node_type: "folder".into(),
            ..node(node_id, parent_id, name)
        }
    }

    fn test_mount() -> MountState {
        MountState {
            path: "/sync".to_owned(),
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
            error: None,
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(Notify::new()),
        }
    }

    fn test_state(backend_url: String) -> DaemonState {
        let mount = test_mount();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
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
        DaemonState {
            paused: Arc::new(AtomicBool::new(false)),
            fs_events_paused: Arc::new(AtomicBool::new(false)),
            mounts: Arc::new(Mutex::new(vec![mount])),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            account: Arc::new(Mutex::new(None)),
            update_status: Arc::new(Mutex::new(Default::default())),
            backend_health: Arc::new(crate::BackendHealth::default()),
            pending_uploads: Arc::new(Mutex::new(std::collections::HashSet::new())),
            deferred_deletes: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(Mutex::new(conn)),
            client: reqwest::Client::new(),
            config: DaemonConfig {
                backend_url,
                device_id: "device-1".to_owned(),
                device_token: "token".to_owned(),
                device_name: "Test Device".to_owned(),
                mounts: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn fp_delete_conflict_response_has_json_body() {
        let backend_url = backend_url_with_response(
            "HTTP/1.1 200 OK",
            r#"{"result":"superseded","current_seq":8}"#,
        )
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", None, "doc.txt")).unwrap();
        }

        let error = fp_delete(
            State(state),
            Json(FpDeleteRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 1,
            }),
        )
        .await
        .unwrap_err();
        let response = error.into_response();
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "superseded");
    }

    #[tokio::test]
    async fn fp_delete_update_required_marks_status_and_blocks_follow_up() {
        let (backend_url, requests) = backend_url_recording_responses(vec![
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"future","server_seq":8,"node_id":"node-1"}"#,
            ),
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"applied","server_seq":9,"node_id":"node-1"}"#,
            ),
        ])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", None, "doc.txt")).unwrap();
        }

        let first_error = fp_delete(
            State(state.clone()),
            Json(FpDeleteRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
            }),
        )
        .await
        .unwrap_err();
        let (first_status, first_body) = response_json(first_error).await;
        assert_eq!(first_status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(first_body["error"], "update_required");

        let status = daemon_status(state.clone()).await;
        assert!(status.update_required);
        assert!(status.mounts[0].update_required);

        let second_error = fp_delete(
            State(state),
            Json(FpDeleteRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
            }),
        )
        .await
        .unwrap_err();
        let (second_status, second_body) = response_json(second_error).await;
        assert_eq!(second_status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(second_body["error"], "update_required");
        assert_eq!(requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn fp_move_applied_rename_updates_mirror() {
        let (backend_url, requests) = backend_url_recording_responses(vec![(
            "HTTP/1.1 200 OK",
            r#"{"result":"applied","server_seq":8,"node_id":"node-1"}"#,
        )])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
        }

        let response = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("renamed.txt".to_owned()),
                new_parent_id: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 8);
        let conn = state.db.lock().await;
        let updated = node_store::get_node(&conn, "node-1").unwrap().unwrap();
        assert_eq!(updated.name, "renamed.txt");
        assert_eq!(updated.parent_id.as_deref(), Some("parent-1"));
        assert_eq!(updated.server_seq, 8);
        let captured = requests.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["op_type"], "rename");
        assert_eq!(captured[0]["based_on_seq"], 7);
        assert_eq!(captured[0]["payload"]["new_name"], "renamed.txt");
    }

    #[tokio::test]
    async fn fp_move_update_required_marks_status_and_blocks_follow_up() {
        let (backend_url, requests) = backend_url_recording_responses(vec![
            (
                "HTTP/1.1 426 Upgrade Required",
                r#"{"error":"protocol_too_old","min_protocol":2,"message":"Update Valv"}"#,
            ),
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"applied","server_seq":9,"node_id":"node-1"}"#,
            ),
        ])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
        }

        let first_error = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("renamed.txt".to_owned()),
                new_parent_id: None,
            }),
        )
        .await
        .unwrap_err();
        let (first_status, first_body) = response_json(first_error).await;
        assert_eq!(first_status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(first_body["min_protocol"], 2);

        let status = daemon_status(state.clone()).await;
        assert!(status.update_required);
        assert!(status.mounts[0].update_required);

        let second_error = fp_move(
            State(state),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("again.txt".to_owned()),
                new_parent_id: None,
            }),
        )
        .await
        .unwrap_err();
        let (second_status, second_body) = response_json(second_error).await;
        assert_eq!(second_status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(second_body["error"], "update_required");
        assert_eq!(requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn fp_move_applied_move_updates_parent() {
        let (backend_url, requests) = backend_url_recording_responses(vec![(
            "HTTP/1.1 200 OK",
            r#"{"result":"applied","server_seq":9,"node_id":"node-1"}"#,
        )])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
            node_store::upsert_node(&conn, &folder_node("parent-2", None, "Dest")).unwrap();
        }

        let response = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: None,
                new_parent_id: Some("parent-2".to_owned()),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 9);
        let conn = state.db.lock().await;
        let updated = node_store::get_node(&conn, "node-1").unwrap().unwrap();
        assert_eq!(updated.name, "doc.txt");
        assert_eq!(updated.parent_id.as_deref(), Some("parent-2"));
        assert_eq!(updated.server_seq, 9);
        let captured = requests.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["op_type"], "move");
        assert_eq!(captured[0]["based_on_seq"], 7);
        assert_eq!(captured[0]["payload"]["new_parent_id"], "parent-2");
    }

    #[tokio::test]
    async fn fp_move_cross_folder_destination_rejected_without_backend_call() {
        let (backend_url, requests) = backend_url_recording_responses(vec![(
            "HTTP/1.1 500 Internal Server Error",
            r#"{}"#,
        )])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
            let mut other_folder_parent = folder_node("parent-9", None, "Other Folder Root");
            other_folder_parent.folder_id = "folder-9".to_owned();
            node_store::upsert_node(&conn, &other_folder_parent).unwrap();
        }

        let error = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: None,
                new_parent_id: Some("parent-9".to_owned()),
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = response_json(error).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, json!({ "error": "cross_folder_move_rejected" }));
        assert_eq!(requests.lock().await.len(), 0);
        let conn = state.db.lock().await;
        let unchanged = node_store::get_node(&conn, "node-1").unwrap().unwrap();
        assert_eq!(unchanged.parent_id.as_deref(), Some("parent-1"));
        assert_eq!(unchanged.server_seq, 7);
    }

    #[tokio::test]
    async fn fp_move_destination_not_in_local_mirror_reaches_backend() {
        let (backend_url, requests) = backend_url_recording_responses(vec![(
            "HTTP/1.1 200 OK",
            r#"{"result":"applied","server_seq":9,"node_id":"node-1"}"#,
        )])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
        }

        let response = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: None,
                new_parent_id: Some("not-mounted-locally".to_owned()),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 9);
        let captured = requests.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["op_type"], "move");
        assert_eq!(
            captured[0]["payload"]["new_parent_id"],
            "not-mounted-locally"
        );
    }

    #[tokio::test]
    async fn fp_move_combined_change_chains_move_on_rename_seq() {
        let (backend_url, requests) = backend_url_recording_responses(vec![
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"applied","server_seq":8,"node_id":"node-1"}"#,
            ),
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"applied","server_seq":9,"node_id":"node-1"}"#,
            ),
        ])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
            node_store::upsert_node(&conn, &folder_node("parent-2", None, "Dest")).unwrap();
        }

        let response = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("renamed.txt".to_owned()),
                new_parent_id: Some("parent-2".to_owned()),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.server_seq, 9);
        let conn = state.db.lock().await;
        let updated = node_store::get_node(&conn, "node-1").unwrap().unwrap();
        assert_eq!(updated.name, "renamed.txt");
        assert_eq!(updated.parent_id.as_deref(), Some("parent-2"));
        assert_eq!(updated.server_seq, 9);
        let captured = requests.lock().await;
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["op_type"], "rename");
        assert_eq!(captured[0]["based_on_seq"], 7);
        assert_eq!(captured[1]["op_type"], "move");
        assert_eq!(captured[1]["based_on_seq"], 8);
    }

    #[tokio::test]
    async fn fp_move_combined_rename_applied_then_move_superseded_persists_rename() {
        let (backend_url, requests) = backend_url_recording_responses(vec![
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"applied","server_seq":8,"node_id":"node-1"}"#,
            ),
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"superseded","current_seq":10}"#,
            ),
        ])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
            node_store::upsert_node(&conn, &folder_node("parent-2", None, "Dest")).unwrap();
        }

        let error = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("renamed.txt".to_owned()),
                new_parent_id: Some("parent-2".to_owned()),
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = response_json(error).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, json!({ "error": "superseded", "current_seq": 10 }));
        let conn = state.db.lock().await;
        let updated = node_store::get_node(&conn, "node-1").unwrap().unwrap();
        assert_eq!(updated.name, "renamed.txt");
        assert_eq!(updated.parent_id.as_deref(), Some("parent-1"));
        assert_eq!(updated.server_seq, 8);
        let captured = requests.lock().await;
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["op_type"], "rename");
        assert_eq!(captured[0]["based_on_seq"], 7);
        assert_eq!(captured[1]["op_type"], "move");
        assert_eq!(captured[1]["based_on_seq"], 8);
    }

    #[tokio::test]
    async fn fp_move_superseded_returns_structured_409_and_preserves_mirror() {
        let (backend_url, _requests) = backend_url_recording_responses(vec![(
            "HTTP/1.1 200 OK",
            r#"{"result":"superseded","current_seq":10}"#,
        )])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
        }

        let error = fp_move(
            State(state.clone()),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("renamed.txt".to_owned()),
                new_parent_id: None,
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = response_json(error).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, json!({ "error": "superseded", "current_seq": 10 }));
        let conn = state.db.lock().await;
        let unchanged = node_store::get_node(&conn, "node-1").unwrap().unwrap();
        assert_eq!(unchanged.name, "doc.txt");
        assert_eq!(unchanged.server_seq, 7);
    }

    #[tokio::test]
    async fn fp_move_name_collision_passes_through_backend_409() {
        let (backend_url, _requests) = backend_url_recording_responses(vec![(
            "HTTP/1.1 409 Conflict",
            r#"{"error":"name_collision"}"#,
        )])
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
        }

        let error = fp_move(
            State(state),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: Some("existing.txt".to_owned()),
                new_parent_id: None,
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = response_json(error).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, json!({ "error": "name_collision" }));
    }

    #[tokio::test]
    async fn fp_move_unknown_node_returns_404_without_backend_call() {
        let backend_url =
            backend_url_with_response("HTTP/1.1 500 Internal Server Error", r#"{}"#).await;
        let state = test_state(backend_url);

        let error = fp_move(
            State(state),
            Json(FpMoveRequest {
                node_id: "missing-node".to_owned(),
                based_on_seq: 7,
                new_name: Some("renamed.txt".to_owned()),
                new_parent_id: None,
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = response_json(error).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, json!({ "error": "node_not_found" }));
    }

    #[tokio::test]
    async fn fp_move_rejects_request_with_no_changes() {
        let state = test_state("http://127.0.0.1:1".to_owned());

        let error = fp_move(
            State(state),
            Json(FpMoveRequest {
                node_id: "node-1".to_owned(),
                based_on_seq: 7,
                new_name: None,
                new_parent_id: None,
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = response_json(error).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({ "error": "new_name or new_parent_id is required" })
        );
    }

    #[tokio::test]
    async fn upload_job_failure_sets_mount_status_error() {
        let backend_url = backend_url_with_response(
            "HTTP/1.1 402 Payment Required",
            r#"{"error":"subscription_inactive","status":"none"}"#,
        )
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("parent-1", None, "Parent")).unwrap();
        }

        upload_job(
            state.clone(),
            FpUploadRequest {
                node_id: None,
                parent_id: "parent-1".to_owned(),
                name: "doc.txt".to_owned(),
                based_on_seq: None,
                file_path: "/unused/staged-content".to_owned(),
            },
            "node-new".to_owned(),
        )
        .await;

        let response = crate::control::get_status(State(state))
            .await
            .unwrap()
            .into_response();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status =
            serde_json::from_slice::<valv_sync::protocol::ipc::DaemonStatus>(&bytes).unwrap();

        assert_eq!(
            status.mounts[0].error.as_deref(),
            Some("Sync paused: subscription inactive")
        );
    }

    #[tokio::test]
    async fn fp_upload_update_required_marks_status_and_blocks_follow_up() {
        let (backend_url, requests) = backend_url_recording_responses(vec![
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"future","server_seq":8,"node_id":"node-1"}"#,
            ),
            (
                "HTTP/1.1 200 OK",
                r#"{"result":"applied","server_seq":9,"node_id":"node-1"}"#,
            ),
        ])
        .await;
        let state = test_state(backend_url);
        let staged = std::env::temp_dir().join(format!("valvd-upload-{}", Uuid::new_v4()));
        fs::write(&staged, b"").unwrap();
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("parent-1", None, "Parent")).unwrap();
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
        }
        let upload_request = FpUploadRequest {
            node_id: Some("node-1".to_owned()),
            parent_id: "parent-1".to_owned(),
            name: "doc.txt".to_owned(),
            based_on_seq: Some(7),
            file_path: staged.to_string_lossy().to_string(),
        };

        upload_job(state.clone(), upload_request.clone(), "node-1".to_owned()).await;

        let status = daemon_status(state.clone()).await;
        assert!(status.update_required);
        assert!(status.mounts[0].update_required);

        let second_error = fp_upload(State(state.clone()), Json(upload_request))
            .await
            .unwrap_err();
        let (second_status, second_body) = response_json(second_error).await;
        assert_eq!(second_status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(second_body["error"], "update_required");

        assert_eq!(requests.lock().await.len(), 1);
        let _ = fs::remove_file(staged);
    }

    #[tokio::test]
    async fn upload_job_failure_does_not_end_concurrent_sync() {
        let backend_url = backend_url_with_response(
            "HTTP/1.1 402 Payment Required",
            r#"{"error":"subscription_inactive","status":"none"}"#,
        )
        .await;
        let state = test_state(backend_url);
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("parent-1", None, "Parent")).unwrap();
        }
        let sync_lock = {
            let mounts = state.mounts.lock().await;
            mounts[0].sync_lock.clone()
        };
        let sync_guard = sync_lock.lock().await;
        {
            let mut mounts = state.mounts.lock().await;
            mounts[0].active_syncs = 1;
            mounts[0].error = Some("sync still running".to_owned());
        }

        let upload = tokio::spawn(upload_job(
            state.clone(),
            FpUploadRequest {
                node_id: None,
                parent_id: "parent-1".to_owned(),
                name: "doc.txt".to_owned(),
                based_on_seq: None,
                file_path: "/unused/staged-content".to_owned(),
            },
            "node-new".to_owned(),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        {
            let mounts = state.mounts.lock().await;
            assert_eq!(mounts[0].active_syncs, 1);
            assert_eq!(mounts[0].error.as_deref(), Some("sync still running"));
            assert!(mounts[0].status().syncing);
        }

        drop(sync_guard);
        upload.await.unwrap();

        let mounts = state.mounts.lock().await;
        assert_eq!(mounts[0].active_syncs, 1);
        assert_eq!(
            mounts[0].error.as_deref(),
            Some("Sync paused: subscription inactive")
        );
        assert!(mounts[0].status().syncing);
    }

    #[tokio::test]
    async fn upload_job_success_clears_stale_mount_error() {
        let backend_url = backend_url_with_response(
            "HTTP/1.1 200 OK",
            r#"{"result":"applied","server_seq":8,"node_id":"node-1"}"#,
        )
        .await;
        let state = test_state(backend_url);
        let staged = std::env::temp_dir().join(format!("valvd-upload-{}", Uuid::new_v4()));
        fs::write(&staged, b"").unwrap();
        {
            let conn = state.db.lock().await;
            node_store::upsert_node(&conn, &node("parent-1", None, "Parent")).unwrap();
            node_store::upsert_node(&conn, &node("node-1", Some("parent-1"), "doc.txt")).unwrap();
            let mut mounts = state.mounts.lock().await;
            mounts[0].error = Some("old upload failure".to_owned());
        }

        upload_job(
            state.clone(),
            FpUploadRequest {
                node_id: Some("node-1".to_owned()),
                parent_id: "parent-1".to_owned(),
                name: "doc.txt".to_owned(),
                based_on_seq: Some(7),
                file_path: staged.to_string_lossy().to_string(),
            },
            "node-1".to_owned(),
        )
        .await;

        let mounts = state.mounts.lock().await;
        assert_eq!(mounts[0].error, None);
        let _ = fs::remove_file(staged);
    }

    #[tokio::test]
    async fn submit_op_daemon_preserves_forbidden_backend_body() {
        let backend_url =
            backend_url_with_response("HTTP/1.1 403 Forbidden", r#"{"error":"forbidden"}"#).await;

        let error = submit_op_daemon(
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "folder-1",
            &SubmitOpRequest::Delete {
                node_id: "node-1".to_owned(),
                based_on_seq: 1,
                payload: DeletePayload {},
            },
        )
        .await
        .unwrap_err();

        let (status, body) = response_json(error).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body, json!({ "error": "forbidden" }));
    }

    #[tokio::test]
    async fn submit_op_daemon_sends_protocol_header() {
        let (backend_url, server) = backend_url_with_response_and_request(
            "HTTP/1.1 200 OK",
            r#"{"result":"applied","server_seq":2,"node_id":"node-1"}"#,
        )
        .await;

        let response = submit_op_daemon(
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "folder-1",
            &SubmitOpRequest::Delete {
                node_id: "node-1".to_owned(),
                based_on_seq: 1,
                payload: DeletePayload {},
            },
        )
        .await
        .unwrap();
        let request = server.await.unwrap();

        assert!(matches!(response, SubmitOpResponse::Applied { .. }));
        assert!(request.contains("x-valv-protocol: 1") || request.contains("X-Valv-Protocol: 1"));
    }

    #[tokio::test]
    async fn submit_op_daemon_unknown_result_returns_update_required() {
        let backend_url = backend_url_with_response(
            "HTTP/1.1 200 OK",
            r#"{"result":"future","server_seq":2,"node_id":"node-1"}"#,
        )
        .await;

        let error = submit_op_daemon(
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "folder-1",
            &SubmitOpRequest::Delete {
                node_id: "node-1".to_owned(),
                based_on_seq: 1,
                payload: DeletePayload {},
            },
        )
        .await
        .unwrap_err();

        let (status, body) = response_json(error).await;
        assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(body["error"], "update_required");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("unrecognized op submission result"));
    }

    #[tokio::test]
    async fn submit_op_daemon_426_returns_update_required_with_min_protocol() {
        let backend_url = backend_url_with_response(
            "HTTP/1.1 426 Upgrade Required",
            r#"{"error":"protocol_too_old","min_protocol":2,"message":"Update Valv"}"#,
        )
        .await;

        let error = submit_op_daemon(
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "folder-1",
            &SubmitOpRequest::Delete {
                node_id: "node-1".to_owned(),
                based_on_seq: 1,
                payload: DeletePayload {},
            },
        )
        .await
        .unwrap_err();

        let (status, body) = response_json(error).await;
        assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
        assert_eq!(body["error"], "update_required");
        assert_eq!(body["min_protocol"], 2);
        assert_eq!(body["message"], "Update Valv");
    }

    #[tokio::test]
    async fn empty_folder_id_query_returns_bad_request() {
        let error = fp_anchor(
            State(test_state("http://127.0.0.1:1".to_owned())),
            Query(FpFolderQuery {
                folder_id: Some(String::new()),
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DaemonError::BadRequest(_)));
    }
}
