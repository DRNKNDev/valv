use std::{fs, path::Path};

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
use sha2::{Digest, Sha256};
use uuid::Uuid;
use valv_sync::{
    chunking::{chunk_file, Chunk},
    persistence::{chunks as chunk_store, mounts, nodes, versions, LocalNode},
    protocol::{
        http::{BatchOperation, BatchRequest, BatchRequestObject, BatchResponse},
        ipc::{
            FpAnchorResponse, FpChangesResponse, FpChunkDownload, FpContentResponse,
            FpDeleteRequest, FpEnumerateResponse, FpItem, FpShareRequest, FpShareResponse,
            FpUploadQueued, FpUploadRequest,
        },
        sync::{
            ChunkRef, CreatePayload, DeletePayload, NewVersionPayload, NodeType, SubmitOpRequest,
            SubmitOpResponse,
        },
    },
    sync_engine::op_submit::submit_op,
};

use crate::{internal_error, DaemonState, ErrorResponse, MountState};

pub(crate) async fn fp_items(
    State(state): State<DaemonState>,
    Query(query): Query<FpItemsQuery>,
) -> Result<Json<FpEnumerateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref())
        .await
        .map_err(internal_error)?;
    let limit = query.limit.unwrap_or(200).min(200);
    let offset = query.offset.unwrap_or(0);
    let conn = state.db.lock().await;
    let parent = resolve_parent_id(&conn, &mount, &query.parent).map_err(internal_error)?;
    let (nodes, total) =
        nodes::list_children(&conn, parent.as_deref(), &mount.folder_id, offset, limit)
            .map_err(internal_error)?;
    let items = nodes
        .iter()
        .map(|node| fp_item_from_node(&conn, node))
        .collect::<Result<Vec<_>>>()
        .map_err(internal_error)?;
    let synced_to_seq = mounts::get_cursor(&conn, &mount.folder_id).map_err(internal_error)?;
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
) -> Result<Json<FpItem>, (StatusCode, Json<ErrorResponse>)> {
    let conn = state.db.lock().await;
    let Some(node) = nodes::get_node(&conn, &node_id).map_err(internal_error)? else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("node_not_found")),
        ));
    };
    Ok(Json(
        fp_item_from_node(&conn, &node).map_err(internal_error)?,
    ))
}

pub(crate) async fn fp_anchor(
    State(state): State<DaemonState>,
    Query(query): Query<FpFolderQuery>,
) -> Result<Json<FpAnchorResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref())
        .await
        .map_err(internal_error)?;
    let conn = state.db.lock().await;
    Ok(Json(FpAnchorResponse {
        server_seq: mounts::get_cursor(&conn, &mount.folder_id).map_err(internal_error)?,
        can_write: mount.can_write,
    }))
}

const FP_WATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub(crate) async fn fp_watch(
    State(state): State<DaemonState>,
    Query(query): Query<FpWatchQuery>,
) -> Result<Json<FpAnchorResponse>, (StatusCode, Json<ErrorResponse>)> {
    fp_watch_inner(state, query, FP_WATCH_TIMEOUT).await
}

async fn fp_watch_inner(
    state: DaemonState,
    query: FpWatchQuery,
    timeout: std::time::Duration,
) -> Result<Json<FpAnchorResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Ok(mount) = resolve_mount_for_query(&state, query.folder_id.as_deref()).await else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("mount_not_found")),
        ));
    };
    // Register as a waiter before checking the cursor: `Notify::notified()`'s
    // future, once created, captures any `notify_waiters()` call that happens
    // after this point even if we haven't started awaiting it yet - checking
    // the cursor first and only then calling `.notified()` would leave a race
    // where a cursor advance between the check and the call is silently missed.
    let notified = mount.cursor_notify.notified();
    let current_seq = {
        let conn = state.db.lock().await;
        mounts::get_cursor(&conn, &mount.folder_id).map_err(internal_error)?
    };
    if current_seq <= query.since_seq {
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(timeout) => {}
        }
    }
    let server_seq = {
        let conn = state.db.lock().await;
        mounts::get_cursor(&conn, &mount.folder_id).map_err(internal_error)?
    };
    Ok(Json(FpAnchorResponse {
        server_seq,
        can_write: mount.can_write,
    }))
}

pub(crate) async fn fp_changes(
    State(state): State<DaemonState>,
    Query(query): Query<FpChangesQuery>,
) -> Result<Json<FpChangesResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mount = resolve_mount_for_query(&state, query.folder_id.as_deref())
        .await
        .map_err(internal_error)?;
    let conn = state.db.lock().await;
    let nodes = nodes::list_changed_since(&conn, &mount.folder_id, query.since_seq.unwrap_or(0))
        .map_err(internal_error)?;
    let items = nodes
        .iter()
        .map(|node| fp_item_from_node(&conn, node))
        .collect::<Result<Vec<_>>>()
        .map_err(internal_error)?;
    let current_seq = mounts::get_cursor(&conn, &mount.folder_id).map_err(internal_error)?;
    Ok(Json(FpChangesResponse {
        items,
        current_seq,
        more_coming: false,
    }))
}

pub(crate) async fn fp_content(
    State(state): State<DaemonState>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<FpContentResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (mount, version) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &node_id).map_err(internal_error)? else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new("node_not_found")),
            ));
        };
        let Some(version_id) = node.current_version_id.as_deref() else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new("version_not_found")),
            ));
        };
        let Some(version) = versions::get_version(&conn, version_id).map_err(internal_error)?
        else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new("version_not_found")),
            ));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id)
            .await
            .map_err(internal_error)?;
        (mount, version)
    };
    let manifest =
        serde_json::from_str::<Vec<ChunkRef>>(&version.manifest_json).map_err(internal_error)?;
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
        .await
        .map_err(internal_error)?
        .error_for_status()
        .map_err(internal_error)?
        .json::<BatchResponse>()
        .await
        .map_err(internal_error)?;
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
        .collect::<Result<Vec<_>>>()
        .map_err(internal_error)?;

    Ok(Json(FpContentResponse {
        version_id: version.version_id,
        size_bytes: version.size_bytes,
        chunks,
    }))
}

pub(crate) async fn fp_upload(
    State(state): State<DaemonState>,
    Json(req): Json<FpUploadRequest>,
) -> Result<(StatusCode, Json<FpUploadQueued>), (StatusCode, Json<ErrorResponse>)> {
    let node_id = req
        .node_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    tokio::spawn(upload_job(state, req, node_id.clone()));
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
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let (folder_id, token) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &req.node_id).map_err(internal_error)? else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new("node_not_found")),
            ));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id)
            .await
            .map_err(internal_error)?;
        (
            node.folder_id,
            mount.effective_token(&state.config).to_owned(),
        )
    };
    let response = submit_op(
        &state.client,
        &state.config.backend_url,
        &token,
        &folder_id,
        &SubmitOpRequest::Delete {
            node_id: req.node_id,
            based_on_seq: req.based_on_seq,
            payload: DeletePayload {},
        },
    )
    .await
    .map_err(internal_error)?;
    Ok(match response {
        SubmitOpResponse::Applied { .. } => StatusCode::NO_CONTENT,
        SubmitOpResponse::Superseded { .. } => StatusCode::CONFLICT,
        SubmitOpResponse::ConflictCopy { .. } => StatusCode::CONFLICT,
    })
}

pub(crate) async fn fp_share(
    State(state): State<DaemonState>,
    Json(req): Json<FpShareRequest>,
) -> Result<Json<FpShareResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (folder_id, token) = {
        let conn = state.db.lock().await;
        let Some(node) = nodes::get_node(&conn, &req.node_id).map_err(internal_error)? else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new("node_not_found")),
            ));
        };
        let mount = resolve_mount_for_folder_id(&state, &node.folder_id)
            .await
            .map_err(internal_error)?;
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
        })
        .send()
        .await
        .map_err(internal_error)?
        .error_for_status()
        .map_err(internal_error)?
        .json::<InviteCreateResponse>()
        .await
        .map_err(internal_error)?;
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
}

#[derive(Debug, Deserialize)]
struct InviteCreateResponse {
    invite_token: String,
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
) -> Result<MountState> {
    let mounts = state.mounts.lock().await;
    if let Some(folder_id) = folder_id {
        mounts
            .iter()
            .find(|mount| mount.folder_id == folder_id)
            .cloned()
            .ok_or_else(|| anyhow!("mount not found for folder {folder_id}"))
    } else {
        match mounts.as_slice() {
            [mount] => Ok(mount.clone()),
            [] => Err(anyhow!("no mounted folders")),
            _ => Err(anyhow!(
                "folder_id is required when multiple folders are mounted"
            )),
        }
    }
}

async fn resolve_mount_for_folder_id(state: &DaemonState, folder_id: &str) -> Result<MountState> {
    state
        .mounts
        .lock()
        .await
        .iter()
        .find(|mount| mount.folder_id == folder_id)
        .cloned()
        .ok_or_else(|| anyhow!("mount not found for folder {folder_id}"))
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
        name: node.name.clone(),
        node_type: node.node_type.clone(),
        version_id: node.current_version_id.clone(),
        content_hash,
        size_bytes,
        server_seq: node.server_seq,
        deleted: node.deleted_at.is_some(),
    })
}

async fn upload_job(state: DaemonState, req: FpUploadRequest, node_id: String) {
    if let Err(error) = upload_job_inner(&state, req, node_id).await {
        eprintln!("file provider upload failed: {error}");
    }
}

async fn upload_job_inner(
    state: &DaemonState,
    req: FpUploadRequest,
    node_id: String,
) -> Result<()> {
    let (folder_id, token, create_first, based_on_seq, parent_id, cursor_notify) = {
        let conn = state.db.lock().await;
        let (mount, parent_id, parent) = if req.parent_id == "root" {
            let mount = resolve_mount_for_query(state, None).await?;
            let parent_id = resolve_parent_id(&conn, &mount, &req.parent_id)?
                .ok_or_else(|| anyhow!("parent node not found: {}", req.parent_id))?;
            let parent = nodes::get_node(&conn, &parent_id)?
                .ok_or_else(|| anyhow!("parent node not found: {parent_id}"))?;
            (mount, parent_id, parent)
        } else {
            let parent = nodes::get_node(&conn, &req.parent_id)?
                .ok_or_else(|| anyhow!("parent node not found: {}", req.parent_id))?;
            let mount = resolve_mount_for_folder_id(state, &parent.folder_id).await?;
            (mount, req.parent_id.clone(), parent)
        };
        let existing = nodes::get_node(&conn, &node_id)?.or(nodes::get_node_by_parent_and_name(
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
        (
            parent.folder_id,
            mount.effective_token(&state.config).to_owned(),
            create_first,
            based_on_seq,
            parent_id,
            mount.cursor_notify.clone(),
        )
    };

    let based_on_seq = if create_first {
        match submit_op(
            &state.client,
            &state.config.backend_url,
            &token,
            &folder_id,
            &SubmitOpRequest::Create {
                payload: CreatePayload {
                    node_id: node_id.clone(),
                    parent_id: parent_id.clone(),
                    name: req.name.clone(),
                    node_type: NodeType::File,
                },
            },
        )
        .await?
        {
            SubmitOpResponse::Applied { server_seq, .. } => server_seq,
            SubmitOpResponse::Superseded { .. } => {
                return Err(anyhow!("create op was superseded for {node_id}"));
            }
            SubmitOpResponse::ConflictCopy { .. } => based_on_seq,
        }
    } else {
        based_on_seq
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
    upload_pending_chunks(&state.client, &state.config.backend_url, &token, &pending).await?;
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
    let response = submit_op(
        &state.client,
        &state.config.backend_url,
        &token,
        &folder_id,
        &SubmitOpRequest::NewVersion {
            node_id,
            based_on_seq,
            payload: NewVersionPayload {
                version_id: Uuid::new_v4().to_string(),
                content_hash: manifest_content_hash(&manifest),
                size_bytes: chunks.iter().map(|chunk| chunk.length).sum(),
                manifest,
            },
        },
    )
    .await?;
    if matches!(response, SubmitOpResponse::ConflictCopy { .. }) {
        materialize_conflict_copy_name(path, &state.config.device_name)?;
    }
    cursor_notify.notify_waiters();
    Ok(())
}

async fn upload_pending_chunks(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    chunks: &[Chunk],
) -> Result<()> {
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
        .await?
        .error_for_status()?
        .json::<BatchResponse>()
        .await?;
    for object in batch.objects {
        if let Some(error) = object.error {
            return Err(anyhow!(
                "batch upload error for {}: {}",
                object.oid,
                error.message
            ));
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
        request.send().await?.error_for_status()?;
    }
    Ok(())
}

fn manifest_content_hash(manifest: &[ChunkRef]) -> String {
    let mut hasher = Sha256::new();
    for chunk in manifest {
        hasher.update(chunk.chunk_hash.as_bytes());
    }
    hex::encode(hasher.finalize())
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
    fs::copy(path, path.with_file_name(conflict_name))?;
    Ok(())
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
            error: None,
            sync_lock: Arc::new(Mutex::new(())),
            cursor_notify: Arc::new(Notify::new()),
        }
    }

    fn test_state(mount: MountState) -> DaemonState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!(
            "../../valv-sync/src/persistence/schema.sql"
        ))
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

        let (status, _) = fp_watch(
            State(state),
            Query(FpWatchQuery {
                folder_id: Some("unknown-folder".into()),
                since_seq: 0,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
