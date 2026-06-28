use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::Local;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use reqwest::header::{HeaderName, HeaderValue};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tokio::{
    sync::{mpsc, Mutex},
    time::sleep,
};
use uuid::Uuid;

use crate::{
    chunking::{chunk_file, Chunk},
    persistence::{
        chunks as chunk_store,
        nodes::{get_node_by_parent_and_name, get_root_node, LocalNode},
    },
    protocol::http::{BatchOperation, BatchRequest, BatchRequestObject, BatchResponse},
    protocol::sync::{
        ChunkRef, CreatePayload, DeletePayload, MovePayload, NewVersionPayload, NodeType,
        RenamePayload, SubmitOpRequest, SubmitOpResponse,
    },
    sync_engine::op_submit::{materialize_conflict_copy, submit_op},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    Create(PathBuf),
    Modify(PathBuf),
    Delete(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchMount {
    pub path: PathBuf,
    pub folder_id: String,
    pub device_name: String,
}

pub async fn fs_watch_task(
    mount: WatchMount,
    paused: Arc<AtomicBool>,
    db: Arc<Mutex<Connection>>,
    client: reqwest::Client,
    backend_url: String,
    token: String,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(256);
    watch_mount(&mount.path, tx)?;

    while let Some(events) = debounce_next_batch(&mut rx).await {
        if paused.load(Ordering::Acquire) {
            continue;
        }

        for event in events {
            handle_fs_event(&mount, &db, &client, &backend_url, &token, event).await?;
        }
    }

    Ok(())
}

pub fn watch_mount(path: &Path, tx: mpsc::Sender<FsEvent>) -> Result<()> {
    let sender = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<Event>| {
            let Ok(event) = result else {
                return;
            };
            if let Some(fs_event) = map_notify_event(event) {
                let _ = sender.blocking_send(fs_event);
            }
        },
        Config::default(),
    )?;
    watcher.watch(path, RecursiveMode::Recursive)?;

    // notify drops its OS watcher when the Rust watcher is dropped. The daemon
    // owns watcher lifetime at process scope, so intentionally keep it alive.
    Box::leak(Box::new(watcher));
    Ok(())
}

pub async fn debounce_next_batch(rx: &mut mpsc::Receiver<FsEvent>) -> Option<Vec<FsEvent>> {
    let first = rx.recv().await?;
    sleep(Duration::from_millis(150)).await;

    let mut events = vec![first];
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    Some(collapse_events(events))
}

pub fn collapse_events(events: Vec<FsEvent>) -> Vec<FsEvent> {
    let mut collapsed = HashMap::<PathBuf, FsEvent>::new();
    let mut renames = Vec::new();

    for event in events {
        match event {
            FsEvent::Rename { from, to } => renames.push(FsEvent::Rename { from, to }),
            FsEvent::Create(path) => {
                collapsed.insert(path.clone(), FsEvent::Create(path));
            }
            FsEvent::Modify(path) => match collapsed.get(&path) {
                Some(FsEvent::Create(_)) => {}
                _ => {
                    collapsed.insert(path.clone(), FsEvent::Modify(path));
                }
            },
            FsEvent::Delete(path) => {
                collapsed.insert(path.clone(), FsEvent::Delete(path));
            }
        }
    }

    let mut output = collapsed.into_values().collect::<Vec<_>>();
    output.extend(renames);
    output
}

pub fn resolve_abs_path(
    conn: &Connection,
    mount_root: &Path,
    folder_id: &str,
    abs_path: &Path,
) -> Result<Option<LocalNode>> {
    let rel = abs_path.strip_prefix(mount_root).with_context(|| {
        format!(
            "{} is outside mount {}",
            abs_path.display(),
            mount_root.display()
        )
    })?;
    let Some(root) = get_root_node(conn, folder_id)? else {
        return Ok(None);
    };
    if rel.as_os_str().is_empty() {
        return Ok(Some(root));
    }

    let mut parent_id = root.node_id;
    let mut node = None;
    for component in rel.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(anyhow!(
                "unsupported path component in {}",
                abs_path.display()
            ));
        };
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 path component in {}", abs_path.display()))?;
        let Some(next) = get_node_by_parent_and_name(conn, folder_id, Some(&parent_id), name)?
        else {
            return Ok(None);
        };
        parent_id = next.node_id.clone();
        node = Some(next);
    }

    Ok(node)
}

fn resolve_parent_for_path(
    conn: &Connection,
    mount_root: &Path,
    folder_id: &str,
    abs_path: &Path,
) -> Result<Option<LocalNode>> {
    let Some(parent) = abs_path.parent() else {
        return Ok(None);
    };
    resolve_abs_path(conn, mount_root, folder_id, parent)
}

async fn handle_fs_event(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    event: FsEvent,
) -> Result<()> {
    match event {
        FsEvent::Create(path) => handle_create(mount, db, client, backend_url, token, &path).await,
        FsEvent::Modify(path) => handle_modify(mount, db, client, backend_url, token, &path).await,
        FsEvent::Delete(path) => handle_delete(mount, db, client, backend_url, token, &path).await,
        FsEvent::Rename { from, to } => {
            handle_rename(mount, db, client, backend_url, token, &from, &to).await
        }
    }
}

async fn handle_create(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    path: &Path,
) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let node_type = if metadata.is_dir() {
        NodeType::Folder
    } else {
        NodeType::File
    };
    let name = file_name(path)?;
    let req = {
        let conn = db.lock().await;
        let Some(parent) = resolve_parent_for_path(&conn, &mount.path, &mount.folder_id, path)?
        else {
            return Ok(());
        };
        SubmitOpRequest::Create {
            payload: CreatePayload {
                node_id: Uuid::new_v4().to_string(),
                parent_id: parent.node_id,
                name,
                node_type,
            },
        }
    };
    submit_op(client, backend_url, token, &mount.folder_id, &req).await?;
    Ok(())
}

async fn handle_modify(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    path: &Path,
) -> Result<()> {
    let node = {
        let conn = db.lock().await;
        let Some(node) = resolve_abs_path(&conn, &mount.path, &mount.folder_id, path)? else {
            return Ok(());
        };
        node
    };
    if node.node_type == "file" {
        upload_file_new_version(
            mount,
            db,
            client,
            backend_url,
            token,
            &node.node_id,
            node.server_seq,
            path,
        )
        .await?;
    }
    Ok(())
}

async fn upload_file_new_version(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    node_id: &str,
    based_on_seq: i64,
    path: &Path,
) -> Result<()> {
    let chunks = chunk_file(path)?;
    let pending = {
        let conn = db.lock().await;
        chunks
            .iter()
            .filter_map(|chunk| match chunk_store::is_uploaded(&conn, &chunk.hash) {
                Ok(true) => None,
                Ok(false) => Some(Ok(chunk.clone())),
                Err(err) => Some(Err(err)),
            })
            .collect::<Result<Vec<_>>>()?
    };
    upload_pending_chunks(client, backend_url, token, &pending).await?;
    {
        let conn = db.lock().await;
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
        client,
        backend_url,
        token,
        &mount.folder_id,
        &SubmitOpRequest::NewVersion {
            node_id: node_id.to_owned(),
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
        let date = Local::now().date_naive().to_string();
        materialize_conflict_copy(path, &mount.device_name, &date)?;
    }
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
            backend_url.trim_end_matches('/')
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

async fn handle_delete(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    path: &Path,
) -> Result<()> {
    let req = {
        let conn = db.lock().await;
        let Some(node) = resolve_abs_path(&conn, &mount.path, &mount.folder_id, path)? else {
            return Ok(());
        };
        if node.parent_id.is_none() {
            return Ok(());
        }
        SubmitOpRequest::Delete {
            node_id: node.node_id,
            based_on_seq: node.server_seq,
            payload: DeletePayload {},
        }
    };
    submit_op(client, backend_url, token, &mount.folder_id, &req).await?;
    Ok(())
}

async fn handle_rename(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    from: &Path,
    to: &Path,
) -> Result<()> {
    let req = {
        let conn = db.lock().await;
        let Some(from_node) = resolve_abs_path(&conn, &mount.path, &mount.folder_id, from)? else {
            return Ok(());
        };
        let Some(to_parent) = resolve_parent_for_path(&conn, &mount.path, &mount.folder_id, to)?
        else {
            return Ok(());
        };
        let Some(from_parent_id) = from_node.parent_id.clone() else {
            return Ok(());
        };

        if from_parent_id == to_parent.node_id {
            SubmitOpRequest::Rename {
                node_id: from_node.node_id,
                based_on_seq: from_node.server_seq,
                payload: RenamePayload {
                    new_name: file_name(to)?,
                },
            }
        } else {
            SubmitOpRequest::Move {
                node_id: from_node.node_id,
                based_on_seq: from_node.server_seq,
                payload: MovePayload {
                    new_parent_id: to_parent.node_id,
                },
            }
        }
    };

    submit_op(client, backend_url, token, &mount.folder_id, &req).await?;
    Ok(())
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("path has no valid UTF-8 file name: {}", path.display()))
}

fn map_notify_event(event: Event) -> Option<FsEvent> {
    match event.kind {
        EventKind::Create(_) => event.paths.into_iter().next().map(FsEvent::Create),
        EventKind::Modify(notify::event::ModifyKind::Name(_)) if event.paths.len() >= 2 => {
            Some(FsEvent::Rename {
                from: event.paths[0].clone(),
                to: event.paths[1].clone(),
            })
        }
        EventKind::Modify(_) => event.paths.into_iter().next().map(FsEvent::Modify),
        EventKind::Remove(_) => event.paths.into_iter().next().map(FsEvent::Delete),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{nodes::upsert_node, schema_sql, LocalNode};

    #[test]
    fn collapse_rapid_modify_events_to_one() {
        let path = PathBuf::from("/sync/report.md");
        let events = vec![
            FsEvent::Modify(path.clone()),
            FsEvent::Modify(path.clone()),
            FsEvent::Modify(path.clone()),
            FsEvent::Modify(path.clone()),
            FsEvent::Modify(path.clone()),
        ];

        assert_eq!(collapse_events(events), vec![FsEvent::Modify(path)]);
    }

    #[test]
    fn collapse_create_then_modify_to_create() {
        let path = PathBuf::from("/sync/report.md");
        let events = vec![FsEvent::Create(path.clone()), FsEvent::Modify(path.clone())];

        assert_eq!(collapse_events(events), vec![FsEvent::Create(path)]);
    }

    #[test]
    fn resolve_abs_path_walks_from_mount_root_node() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema_sql()).unwrap();
        upsert_node(
            &conn,
            &LocalNode {
                node_id: "root-node".into(),
                folder_id: "folder-1".into(),
                parent_id: None,
                name: "Sync".into(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 1,
                deleted_at: None,
            },
        )
        .unwrap();
        upsert_node(
            &conn,
            &LocalNode {
                node_id: "docs-node".into(),
                folder_id: "folder-1".into(),
                parent_id: Some("root-node".into()),
                name: "docs".into(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 2,
                deleted_at: None,
            },
        )
        .unwrap();
        upsert_node(
            &conn,
            &LocalNode {
                node_id: "report-node".into(),
                folder_id: "folder-1".into(),
                parent_id: Some("docs-node".into()),
                name: "report.md".into(),
                node_type: "file".into(),
                current_version_id: None,
                server_seq: 3,
                deleted_at: None,
            },
        )
        .unwrap();

        let node = resolve_abs_path(
            &conn,
            Path::new("/sync"),
            "folder-1",
            Path::new("/sync/docs/report.md"),
        )
        .unwrap()
        .unwrap();

        assert_eq!(node.node_id, "report-node");
    }
}
