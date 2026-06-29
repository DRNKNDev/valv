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
            if matches!(node_type, NodeType::File) && metadata.len() > 0 {
                upload_file_new_version(
                    mount, db, client, backend_url, token, &node_id, server_seq, path,
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
    fn collapse_child_before_parent_orders_parent_first() {
        let parent = PathBuf::from("/sync/docs");
        let child = PathBuf::from("/sync/docs/notes.md");
        // child arrives before parent
        let events = vec![FsEvent::Create(child.clone()), FsEvent::Create(parent.clone())];
        let result = collapse_events(events);
        assert_eq!(result, vec![FsEvent::Create(parent), FsEvent::Create(child)]);
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
        let (backend_url, server) = submit_op_server(vec![SubmitOpResponse::Applied {
            node_id: "server-file".into(),
            server_seq: 42,
        }])
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

        assert_eq!(requests.len(), 1);
        let conn = db.lock().await;
        let node = get_node(&conn, "server-file").unwrap().unwrap();
        assert_eq!(node.server_seq, 42);
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

        assert_eq!(requests.len(), 2);
        let conn = db.lock().await;
        let docs = get_node(&conn, "docs-node").unwrap().unwrap();
        let file = get_node(&conn, "file-node").unwrap().unwrap();
        assert_eq!(docs.parent_id.as_deref(), Some("root-node"));
        assert_eq!(file.parent_id.as_deref(), Some("docs-node"));
        assert_eq!(file.server_seq, 3);
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
    async fn handle_create_does_not_upload_for_new_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        fs::write(&path, b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = submit_op_server(vec![SubmitOpResponse::Applied {
            node_id: "server-file".into(),
            server_seq: 42,
        }])
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

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op_type"], "create");
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
                    let body =
                        serde_json::to_vec(responses.pop_front().as_ref().unwrap()).unwrap();
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
