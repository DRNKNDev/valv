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
        nodes::{self, get_node_by_parent_and_name, get_root_node, LocalNode},
        versions,
    },
    protocol::http::{BatchOperation, BatchRequest, BatchRequestObject, BatchResponse},
    protocol::sync::{
        ChunkRef, CreatePayload, DeletePayload, MovePayload, NewVersionPayload, NodeType,
        RenamePayload, SubmitOpRequest, SubmitOpResponse,
    },
    sync_engine::op_submit::{apply_submitted_new_version, materialize_conflict_copy, submit_op},
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
    fs_events_paused: Arc<AtomicBool>,
    db: Arc<Mutex<Connection>>,
    client: reqwest::Client,
    backend_url: String,
    token: String,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(256);
    watch_mount(&mount.path, tx, paused.clone(), fs_events_paused.clone())?;
    let mut dropped_paused_events = false;

    loop {
        if paused.load(Ordering::Acquire) || fs_events_paused.load(Ordering::Acquire) {
            drain_pending_events(&mut rx);
            dropped_paused_events = true;
            sleep(Duration::from_millis(50)).await;
            continue;
        }

        if dropped_paused_events {
            sleep(Duration::from_millis(150)).await;
            drain_pending_events(&mut rx);
            dropped_paused_events = false;
            continue;
        }

        let Some(events) = debounce_next_batch(&mut rx).await else {
            break;
        };
        if paused.load(Ordering::Acquire) || fs_events_paused.load(Ordering::Acquire) {
            drain_pending_events(&mut rx);
            dropped_paused_events = true;
            continue;
        }

        for event in events {
            handle_fs_event(&mount, &db, &client, &backend_url, &token, event).await?;
        }
    }

    Ok(())
}

fn drain_pending_events(rx: &mut mpsc::Receiver<FsEvent>) {
    while rx.try_recv().is_ok() {}
}

pub fn watch_mount(
    path: &Path,
    tx: mpsc::Sender<FsEvent>,
    paused: Arc<AtomicBool>,
    fs_events_paused: Arc<AtomicBool>,
) -> Result<()> {
    let sender = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<Event>| {
            if paused.load(Ordering::Acquire) || fs_events_paused.load(Ordering::Acquire) {
                return;
            }
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
    output.sort_by_key(|event| match event {
        FsEvent::Create(p) | FsEvent::Modify(p) | FsEvent::Delete(p) => p.components().count(),
        FsEvent::Rename { from, .. } => from.components().count(),
    });
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
    if !metadata.is_dir() && is_conflict_copy_name(&name) {
        return Ok(());
    }
    let (req, parent_id, local_name, local_node_type) = {
        let conn = db.lock().await;
        let Some(parent) = resolve_parent_for_path(&conn, &mount.path, &mount.folder_id, path)?
        else {
            eprintln!(
                "watcher: skipping create for {} - parent not in local mirror (folder: {})",
                path.display(),
                mount.folder_id
            );
            return Ok(());
        };
        (
            SubmitOpRequest::Create {
                payload: CreatePayload {
                    node_id: Uuid::new_v4().to_string(),
                    parent_id: parent.node_id.clone(),
                    name: name.clone(),
                    node_type: node_type.clone(),
                },
            },
            parent.node_id,
            name,
            node_type_str(&node_type).to_owned(),
        )
    };
    let response = submit_op(client, backend_url, token, &mount.folder_id, &req).await?;
    match response {
        SubmitOpResponse::Applied {
            node_id,
            server_seq,
        } => {
            {
                let conn = db.lock().await;
                nodes::upsert_node(
                    &conn,
                    &LocalNode {
                        node_id: node_id.clone(),
                        folder_id: mount.folder_id.clone(),
                        parent_id: Some(parent_id),
                        name: local_name,
                        node_type: local_node_type,
                        current_version_id: None,
                        server_seq,
                        deleted_at: None,
                    },
                )?;
            }
            if matches!(node_type, NodeType::File) {
                upload_file_new_version(
                    mount,
                    db,
                    client,
                    backend_url,
                    token,
                    &node_id,
                    server_seq,
                    path,
                )
                .await?;
            }
        }
        SubmitOpResponse::Superseded { .. } => {
            eprintln!(
                "watcher: skipping create for {} - name collision on server (folder: {})",
                path.display(),
                mount.folder_id
            );
        }
        SubmitOpResponse::ConflictCopy { .. } => {}
    }
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
            eprintln!(
                "watcher: skipping modify for {} - node not in local mirror (folder: {})",
                path.display(),
                mount.folder_id
            );
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
    let size_bytes = chunks.iter().map(|chunk| chunk.length).sum();
    let manifest = chunks
        .iter()
        .map(|chunk| ChunkRef {
            chunk_hash: chunk.hash.clone(),
            offset: chunk.offset,
            length: chunk.length,
        })
        .collect::<Vec<_>>();
    let content_hash = manifest_content_hash(&manifest);
    {
        let conn = db.lock().await;
        if let Some(node) = nodes::get_node(&conn, node_id)? {
            if let Some(version_id) = node.current_version_id.as_deref() {
                if let Some(version) = versions::get_version(&conn, version_id)? {
                    if version.size_bytes == size_bytes && version.content_hash == content_hash {
                        return Ok(());
                    }
                }
            }
        }
    }

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

    let req = SubmitOpRequest::NewVersion {
        node_id: node_id.to_owned(),
        based_on_seq,
        payload: NewVersionPayload {
            version_id: Uuid::new_v4().to_string(),
            content_hash,
            size_bytes,
            manifest,
        },
    };
    let response = submit_op(client, backend_url, token, &mount.folder_id, &req).await?;
    {
        let conn = db.lock().await;
        apply_submitted_new_version(&conn, &mount.folder_id, node_id, &req, &response)?;
    }
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
        .post(format!("{}/objects/batch", crate::api_base(backend_url)))
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

fn is_conflict_copy_name(name: &str) -> bool {
    name.contains(" (conflicted copy, ")
}

async fn handle_delete(
    mount: &WatchMount,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    path: &Path,
) -> Result<()> {
    // notify's single-path `ModifyKind::Name` events are ambiguous: the OS uses
    // them both for genuine out-of-tree moves (e.g. Finder "Move to Trash") and,
    // on some platforms, for one half of an in-tree rename delivered as two
    // separate single-path events instead of one two-path event. In the latter
    // case the path is still present on disk, so trust the filesystem over the
    // event classification before treating this as a real delete.
    if path.exists() {
        return Ok(());
    }
    let (req, node_id) = {
        let conn = db.lock().await;
        let Some(node) = resolve_abs_path(&conn, &mount.path, &mount.folder_id, path)? else {
            eprintln!(
                "watcher: skipping delete for {} - node not in local mirror (folder: {})",
                path.display(),
                mount.folder_id
            );
            return Ok(());
        };
        if node.parent_id.is_none() {
            return Ok(());
        }
        (
            SubmitOpRequest::Delete {
                node_id: node.node_id.clone(),
                based_on_seq: node.server_seq,
                payload: DeletePayload {},
            },
            node.node_id,
        )
    };
    match submit_op(client, backend_url, token, &mount.folder_id, &req).await? {
        SubmitOpResponse::Applied { server_seq, .. } => {
            let conn = db.lock().await;
            nodes::mark_deleted(&conn, &node_id, server_seq)?;
        }
        SubmitOpResponse::Superseded { .. } | SubmitOpResponse::ConflictCopy { .. } => {}
    }
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
    let (req, updated_node) = {
        let conn = db.lock().await;
        let Some(from_node) = resolve_abs_path(&conn, &mount.path, &mount.folder_id, from)? else {
            eprintln!(
                "watcher: skipping rename from {} - node not in local mirror (folder: {})",
                from.display(),
                mount.folder_id
            );
            return Ok(());
        };
        let Some(to_parent) = resolve_parent_for_path(&conn, &mount.path, &mount.folder_id, to)?
        else {
            eprintln!(
                "watcher: skipping rename to {} - parent not in local mirror (folder: {})",
                to.display(),
                mount.folder_id
            );
            return Ok(());
        };
        let Some(from_parent_id) = from_node.parent_id.clone() else {
            return Ok(());
        };
        let to_name = file_name(to)?;
        let node_id = from_node.node_id.clone();
        let to_parent_id = to_parent.node_id.clone();

        let req = if from_parent_id == to_parent.node_id {
            SubmitOpRequest::Rename {
                node_id,
                based_on_seq: from_node.server_seq,
                payload: RenamePayload {
                    new_name: to_name.clone(),
                },
            }
        } else {
            SubmitOpRequest::Move {
                node_id,
                based_on_seq: from_node.server_seq,
                payload: MovePayload {
                    new_parent_id: to_parent_id.clone(),
                },
            }
        };
        let mut updated_node = from_node;
        updated_node.parent_id = Some(to_parent_id);
        updated_node.name = to_name;
        (req, updated_node)
    };

    if let SubmitOpResponse::Applied { server_seq, .. } =
        submit_op(client, backend_url, token, &mount.folder_id, &req).await?
    {
        let conn = db.lock().await;
        nodes::upsert_node(
            &conn,
            &LocalNode {
                server_seq,
                ..updated_node
            },
        )?;
    }
    Ok(())
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("path has no valid UTF-8 file name: {}", path.display()))
}

fn node_type_str(node_type: &NodeType) -> &'static str {
    match node_type {
        NodeType::File => "file",
        NodeType::Folder => "folder",
    }
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
        // 1-path rename: file moved out of the watch tree (e.g. Finder Move to Trash)
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => {
            event.paths.into_iter().next().map(FsEvent::Delete)
        }
        EventKind::Modify(_) => event.paths.into_iter().next().map(FsEvent::Modify),
        EventKind::Remove(_) => event.paths.into_iter().next().map(FsEvent::Delete),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::persistence::{
        nodes::{get_node, upsert_node},
        schema_sql, LocalNode,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
        time::timeout,
    };

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
    fn collapse_delete_after_create_yields_delete() {
        let path = PathBuf::from("/sync/report.md");
        let events = vec![FsEvent::Create(path.clone()), FsEvent::Delete(path.clone())];

        assert_eq!(collapse_events(events), vec![FsEvent::Delete(path)]);
    }

    #[test]
    fn map_notify_event_one_path_name_change_maps_to_delete() {
        let path = PathBuf::from("/sync/file.txt");
        let event = Event::new(EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Any,
        )))
        .add_path(path.clone());

        assert_eq!(map_notify_event(event), Some(FsEvent::Delete(path)));
    }

    #[test]
    fn map_notify_event_two_path_name_change_maps_to_rename() {
        let src = PathBuf::from("/sync/file.txt");
        let dst = PathBuf::from("/sync/renamed.txt");
        let event = Event::new(EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Any,
        )))
        .add_path(src.clone())
        .add_path(dst.clone());

        assert_eq!(
            map_notify_event(event),
            Some(FsEvent::Rename { from: src, to: dst })
        );
    }

    #[test]
    fn map_notify_event_remove_event_maps_to_delete() {
        let path = PathBuf::from("/sync/file.txt");
        let event =
            Event::new(EventKind::Remove(notify::event::RemoveKind::File)).add_path(path.clone());

        assert_eq!(map_notify_event(event), Some(FsEvent::Delete(path)));
    }

    #[test]
    fn collapse_child_before_parent_orders_parent_first() {
        let parent = PathBuf::from("/sync/docs");
        let child = PathBuf::from("/sync/docs/notes.md");
        // child arrives before parent
        let events = vec![
            FsEvent::Create(child.clone()),
            FsEvent::Create(parent.clone()),
        ];
        let result = collapse_events(events);
        assert_eq!(
            result,
            vec![FsEvent::Create(parent), FsEvent::Create(child)]
        );
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

    #[tokio::test]
    async fn handle_create_applied_response_writes_local_node() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        fs::write(&path, b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = submit_op_server(vec![
            SubmitOpResponse::Applied {
                node_id: "server-file".into(),
                server_seq: 42,
            },
            SubmitOpResponse::Applied {
                node_id: "version-id".into(),
                server_seq: 43,
            },
        ])
        .await;

        handle_create(
            &watch_mount(dir.path()),
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            &path,
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(requests.len(), 2);
        let conn = db.lock().await;
        let node = get_node(&conn, "server-file").unwrap().unwrap();
        assert_eq!(node.server_seq, 43);
        assert_eq!(node.parent_id.as_deref(), Some("root-node"));
        assert_eq!(node.name, "report.md");
        assert_eq!(node.node_type, "file");
    }

    #[tokio::test]
    async fn handle_create_with_empty_mirror_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        fs::write(&path, b"hello").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema_sql()).unwrap();
        let db = Arc::new(Mutex::new(conn));

        handle_create(
            &watch_mount(dir.path()),
            &db,
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            "token",
            &path,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn nested_create_uses_directory_written_from_first_create() {
        let dir = tempfile::tempdir().unwrap();
        let docs_path = dir.path().join("docs");
        let file_path = docs_path.join("notes.md");
        fs::create_dir(&docs_path).unwrap();
        fs::write(&file_path, b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = submit_op_server(vec![
            SubmitOpResponse::Applied {
                node_id: "docs-node".into(),
                server_seq: 2,
            },
            SubmitOpResponse::Applied {
                node_id: "file-node".into(),
                server_seq: 3,
            },
            SubmitOpResponse::Applied {
                node_id: "version-id".into(),
                server_seq: 4,
            },
        ])
        .await;
        let mount = watch_mount(dir.path());
        let client = reqwest::Client::new();

        handle_create(&mount, &db, &client, &backend_url, "token", &docs_path)
            .await
            .unwrap();
        handle_create(&mount, &db, &client, &backend_url, "token", &file_path)
            .await
            .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(requests.len(), 3);
        let conn = db.lock().await;
        let docs = get_node(&conn, "docs-node").unwrap().unwrap();
        let file = get_node(&conn, "file-node").unwrap().unwrap();
        assert_eq!(docs.parent_id.as_deref(), Some("root-node"));
        assert_eq!(file.parent_id.as_deref(), Some("docs-node"));
        assert_eq!(file.server_seq, 4);
    }

    #[tokio::test]
    async fn handle_create_uploads_content_for_new_nonempty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        fs::write(&path, b"hello").unwrap();
        let db = seeded_db();
        let (backend_url, server) = full_create_server(vec![
            SubmitOpResponse::Applied {
                node_id: "server-file".into(),
                server_seq: 42,
            },
            SubmitOpResponse::Applied {
                node_id: "version-id".into(),
                server_seq: 43,
            },
        ])
        .await;

        handle_create(
            &watch_mount(dir.path()),
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            &path,
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["op_type"], "create");
        assert_eq!(requests[1]["op_type"], "new_version");
    }

    #[tokio::test]
    async fn handle_create_submits_version_for_new_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        fs::write(&path, b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = full_create_server(vec![
            SubmitOpResponse::Applied {
                node_id: "server-file".into(),
                server_seq: 42,
            },
            SubmitOpResponse::Applied {
                node_id: "version-id".into(),
                server_seq: 43,
            },
        ])
        .await;

        handle_create(
            &watch_mount(dir.path()),
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            &path,
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["op_type"], "create");
        assert_eq!(requests[1]["op_type"], "new_version");
        assert_eq!(requests[1]["payload"]["size_bytes"], 0);
        assert_eq!(
            requests[1]["payload"]["manifest"].as_array().unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn handle_delete_applied_response_marks_local_node_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete-me.txt");
        fs::write(&path, b"").unwrap();
        let db = seeded_db();
        seed_file(&db, "file-node", "delete-me.txt", 4).await;
        fs::remove_file(&path).unwrap();
        let (backend_url, server) = submit_op_server(vec![SubmitOpResponse::Applied {
            node_id: "file-node".into(),
            server_seq: 7,
        }])
        .await;

        handle_delete(
            &watch_mount(dir.path()),
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            &path,
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
        let conn = db.lock().await;
        let node = get_node(&conn, "file-node").unwrap().unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op_type"], "delete");
        assert_eq!(node.server_seq, 7);
        assert!(node.deleted_at.is_some());
    }

    #[tokio::test]
    async fn handle_delete_superseded_response_leaves_local_node_live() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete-me.txt");
        fs::write(&path, b"").unwrap();
        let db = seeded_db();
        seed_file(&db, "file-node", "delete-me.txt", 4).await;
        fs::remove_file(&path).unwrap();
        let (backend_url, server) =
            submit_op_server(vec![SubmitOpResponse::Superseded { current_seq: 99 }]).await;

        handle_delete(
            &watch_mount(dir.path()),
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            &path,
        )
        .await
        .unwrap();
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
        let conn = db.lock().await;
        let node = get_node(&conn, "file-node").unwrap().unwrap();

        assert_eq!(node.server_seq, 4);
        assert!(node.deleted_at.is_none());
    }

    #[tokio::test]
    async fn delete_then_later_create_same_path_submits_fresh_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recreated.txt");
        fs::write(&path, b"").unwrap();
        let db = seeded_db();
        seed_file(&db, "old-node", "recreated.txt", 4).await;
        fs::remove_file(&path).unwrap();
        let (backend_url, server) = submit_op_server(vec![SubmitOpResponse::Applied {
            node_id: "old-node".into(),
            server_seq: 7,
        }])
        .await;
        let mount = watch_mount(dir.path());
        let client = reqwest::Client::new();

        handle_delete(&mount, &db, &client, &backend_url, "token", &path)
            .await
            .unwrap();
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
        {
            let conn = db.lock().await;
            let old = get_node(&conn, "old-node").unwrap().unwrap();
            assert!(old.deleted_at.is_some());
            assert!(resolve_abs_path(&conn, dir.path(), "folder-1", &path)
                .unwrap()
                .is_none());
        }

        fs::write(&path, b"").unwrap();
        let (backend_url, server) = submit_op_server(vec![
            SubmitOpResponse::Applied {
                node_id: "new-node".into(),
                server_seq: 8,
            },
            SubmitOpResponse::Applied {
                node_id: "new-version".into(),
                server_seq: 9,
            },
        ])
        .await;
        handle_create(&mount, &db, &client, &backend_url, "token", &path)
            .await
            .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
        let conn = db.lock().await;
        let old = get_node(&conn, "old-node").unwrap().unwrap();
        let new = get_node(&conn, "new-node").unwrap().unwrap();

        assert_eq!(requests[0]["op_type"], "create");
        assert_eq!(requests[0]["payload"]["name"], "recreated.txt");
        assert!(old.deleted_at.is_some());
        assert_eq!(new.parent_id.as_deref(), Some("root-node"));
        assert_eq!(new.name, "recreated.txt");
    }

    fn seeded_db() -> Arc<Mutex<Connection>> {
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
        Arc::new(Mutex::new(conn))
    }

    async fn seed_file(db: &Arc<Mutex<Connection>>, node_id: &str, name: &str, server_seq: i64) {
        let conn = db.lock().await;
        upsert_node(
            &conn,
            &LocalNode {
                node_id: node_id.into(),
                folder_id: "folder-1".into(),
                parent_id: Some("root-node".into()),
                name: name.into(),
                node_type: "file".into(),
                current_version_id: None,
                server_seq,
                deleted_at: None,
            },
        )
        .unwrap();
    }

    fn watch_mount(path: &Path) -> WatchMount {
        WatchMount {
            path: path.to_path_buf(),
            folder_id: "folder-1".into(),
            device_name: "Test Device".into(),
        }
    }

    async fn parse_http_request_raw(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end;
        loop {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before headers");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = pos + 4;
                break;
            }
        }
        let header_text = String::from_utf8_lossy(&buf[..header_end]);
        let request_line = header_text.lines().next().unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap().to_owned();
        let path = parts.next().unwrap().to_owned();
        let content_length = header_text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, val)| val.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before body");
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(content_length);
        (method, path, body)
    }

    // Handles Create op + objects/batch + chunk PUT + NewVersion op.
    async fn full_create_server(
        responses: Vec<SubmitOpResponse>,
    ) -> (String, JoinHandle<Vec<serde_json::Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let server_url = base_url.clone();
        let server = tokio::spawn(async move {
            let mut responses = VecDeque::from(responses);
            let mut requests = Vec::new();
            while !responses.is_empty() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (method, path, body) = parse_http_request_raw(&mut stream).await;
                if method == "POST" && path.ends_with("/objects/batch") {
                    let batch_req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let objects = batch_req["objects"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|obj| {
                            let oid = obj["oid"].as_str().unwrap();
                            let size = obj["size"].as_u64().unwrap();
                            serde_json::json!({
                                "oid": oid,
                                "size": size,
                                "actions": {
                                    "upload": {
                                        "href": format!("{server_url}/upload/{oid}"),
                                        "header": {}
                                    }
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    let resp = serde_json::to_vec(
                        &serde_json::json!({"transfer": "basic", "objects": objects}),
                    )
                    .unwrap();
                    write_response(&mut stream, "200 OK", &resp).await;
                } else if method == "PUT" {
                    write_response(&mut stream, "204 No Content", b"").await;
                } else if method == "POST" {
                    requests.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
                    let body = serde_json::to_vec(responses.pop_front().as_ref().unwrap()).unwrap();
                    write_response(&mut stream, "200 OK", &body).await;
                } else {
                    write_response(&mut stream, "404 Not Found", b"").await;
                }
            }
            requests
        });
        (base_url, server)
    }

    async fn submit_op_server(
        responses: Vec<SubmitOpResponse>,
    ) -> (String, JoinHandle<Vec<serde_json::Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let server = tokio::spawn(async move {
            let mut responses = VecDeque::from(responses);
            let mut requests = Vec::new();
            while let Some(response) = responses.pop_front() {
                let (mut stream, _) = listener.accept().await.unwrap();
                requests.push(read_request_json(&mut stream).await);
                let body = serde_json::to_vec(&response).unwrap();
                write_response(&mut stream, "200 OK", &body).await;
            }
            requests
        });
        (base_url, server)
    }

    async fn read_request_json(stream: &mut TcpStream) -> serde_json::Value {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end;
        loop {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before headers");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = pos + 4;
                break;
            }
        }

        let header_text = String::from_utf8_lossy(&buf[..header_end]);
        let content_length = header_text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before body");
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(content_length);
        serde_json::from_slice(&body).unwrap()
    }

    async fn write_response(stream: &mut TcpStream, status: &str, body: &[u8]) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
    }
}
