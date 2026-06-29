use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Result};
use chrono::Local;
use rusqlite::Connection;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    persistence::{
        nodes::{self, get_node_by_parent_and_name, get_root_node, LocalNode},
        versions,
    },
    protocol::sync::{CreatePayload, NodeType, SubmitOpRequest, SubmitOpResponse},
    sync_engine::op_submit::{submit_op, upload_then_submit_new_version},
};

#[derive(Debug, Default)]
pub struct PushSummary {
    pub creates_submitted: u64,
    pub versions_submitted: u64,
    pub skipped: u64,
    pub errors: u64,
}

pub async fn push_local(
    mount_root: &Path,
    folder_id: &str,
    scope_node_id: Option<&str>,
    db: &Arc<Mutex<Connection>>,
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    device_name: &str,
) -> Result<PushSummary> {
    let seed_parent_id = {
        let conn = db.lock().await;
        let root = get_root_node(&conn, folder_id)?.ok_or_else(|| {
            anyhow!(
                "no root node in local mirror for folder {}; run 'valv mount' first",
                folder_id
            )
        })?;
        scope_node_id.unwrap_or(&root.node_id).to_owned()
    };

    let mut summary = PushSummary::default();
    let mut queue = VecDeque::<(PathBuf, String)>::new();
    queue.push_back((mount_root.to_path_buf(), seed_parent_id));

    while let Some((dir_path, parent_node_id)) = queue.pop_front() {
        let read_dir = match fs::read_dir(&dir_path) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                eprintln!(
                    "push_local: failed to read directory {}: {error}",
                    dir_path.display()
                );
                summary.errors += 1;
                continue;
            }
        };

        let mut entries = Vec::new();
        for entry in read_dir {
            match entry {
                Ok(entry) => {
                    let abs_path = entry.path();
                    let name = match file_name(&abs_path) {
                        Ok(name) => name,
                        Err(error) => {
                            eprintln!("push_local: skipping {}: {error}", abs_path.display());
                            summary.errors += 1;
                            continue;
                        }
                    };
                    let metadata = match fs::symlink_metadata(&abs_path) {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            eprintln!("push_local: failed to stat {}: {error}", abs_path.display());
                            summary.errors += 1;
                            continue;
                        }
                    };
                    entries.push(EntryInfo {
                        abs_path,
                        name,
                        is_dir: metadata.is_dir(),
                        is_file: metadata.is_file(),
                        is_symlink: metadata.file_type().is_symlink(),
                        len: metadata.len(),
                    });
                }
                Err(error) => {
                    eprintln!(
                        "push_local: failed to read entry in {}: {error}",
                        dir_path.display()
                    );
                    summary.errors += 1;
                }
            }
        }
        entries.sort_by(|left, right| {
            right
                .is_dir
                .cmp(&left.is_dir)
                .then_with(|| left.name.cmp(&right.name))
        });

        for entry in entries {
            if entry.is_symlink {
                eprintln!("push_local: skipping symlink {}", entry.abs_path.display());
                summary.skipped += 1;
                continue;
            }

            let mirror_node = {
                let conn = db.lock().await;
                get_node_by_parent_and_name(&conn, folder_id, Some(&parent_node_id), &entry.name)?
            };

            if entry.is_dir {
                if let Some(node) = mirror_node.filter(|node| node.deleted_at.is_none()) {
                    queue.push_back((entry.abs_path, node.node_id));
                } else {
                    create_entry(
                        &mut summary,
                        &mut queue,
                        CreateEntry {
                            abs_path: entry.abs_path,
                            folder_id,
                            parent_node_id: &parent_node_id,
                            name: entry.name,
                            node_type: NodeType::Folder,
                            file_len: 0,
                            device_name,
                            db,
                            client,
                            backend_url,
                            token,
                        },
                    )
                    .await?;
                }
            } else if entry.is_file {
                if let Some(node) = mirror_node.filter(|node| node.deleted_at.is_none()) {
                    process_existing_file(
                        &mut summary,
                        ExistingFile {
                            abs_path: entry.abs_path,
                            metadata_len: entry.len,
                            folder_id,
                            node,
                            db,
                            client,
                            backend_url,
                            token,
                            device_name,
                        },
                    )
                    .await;
                } else {
                    create_entry(
                        &mut summary,
                        &mut queue,
                        CreateEntry {
                            abs_path: entry.abs_path,
                            folder_id,
                            parent_node_id: &parent_node_id,
                            name: entry.name,
                            node_type: NodeType::File,
                            file_len: entry.len,
                            device_name,
                            db,
                            client,
                            backend_url,
                            token,
                        },
                    )
                    .await?;
                }
            } else {
                eprintln!(
                    "push_local: skipping unsupported entry {}",
                    entry.abs_path.display()
                );
                summary.skipped += 1;
            }
        }
    }

    Ok(summary)
}

struct EntryInfo {
    abs_path: PathBuf,
    name: String,
    is_dir: bool,
    is_file: bool,
    is_symlink: bool,
    len: u64,
}

struct CreateEntry<'a> {
    abs_path: PathBuf,
    folder_id: &'a str,
    parent_node_id: &'a str,
    name: String,
    node_type: NodeType,
    file_len: u64,
    device_name: &'a str,
    db: &'a Arc<Mutex<Connection>>,
    client: &'a reqwest::Client,
    backend_url: &'a str,
    token: &'a str,
}

async fn create_entry(
    summary: &mut PushSummary,
    queue: &mut VecDeque<(PathBuf, String)>,
    entry: CreateEntry<'_>,
) -> Result<()> {
    let local_node_type = node_type_str(&entry.node_type).to_owned();
    let is_dir = matches!(entry.node_type, NodeType::Folder);
    let req = SubmitOpRequest::Create {
        payload: CreatePayload {
            node_id: Uuid::new_v4().to_string(),
            parent_id: entry.parent_node_id.to_owned(),
            name: entry.name.clone(),
            node_type: entry.node_type,
        },
    };

    let response = match submit_op(
        entry.client,
        entry.backend_url,
        entry.token,
        entry.folder_id,
        &req,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            eprintln!(
                "push_local: failed to submit create for {}: {error}",
                entry.abs_path.display()
            );
            summary.errors += 1;
            return Ok(());
        }
    };

    match response {
        SubmitOpResponse::Applied {
            node_id,
            server_seq,
        } => {
            {
                let conn = entry.db.lock().await;
                nodes::upsert_node(
                    &conn,
                    &LocalNode {
                        node_id: node_id.clone(),
                        folder_id: entry.folder_id.to_owned(),
                        parent_id: Some(entry.parent_node_id.to_owned()),
                        name: entry.name,
                        node_type: local_node_type,
                        current_version_id: None,
                        server_seq,
                        deleted_at: None,
                    },
                )?;
            }
            summary.creates_submitted += 1;
            if is_dir {
                queue.push_back((entry.abs_path, node_id));
            } else if entry.file_len > 0 {
                let date = today_date_str();
                let conn = entry.db.lock().await;
                match upload_then_submit_new_version(
                    entry.client,
                    entry.backend_url,
                    entry.token,
                    &conn,
                    entry.folder_id,
                    &node_id,
                    server_seq,
                    &entry.abs_path,
                    entry.device_name,
                    &date,
                )
                .await
                {
                    Ok(_) => summary.versions_submitted += 1,
                    Err(error) => {
                        eprintln!(
                            "push_local: failed to upload content for {}: {error}",
                            entry.abs_path.display()
                        );
                        summary.errors += 1;
                    }
                }
            }
        }
        SubmitOpResponse::Superseded { .. } => {
            eprintln!(
                "push_local: name conflict for {}, skipping until next pull",
                entry.abs_path.display()
            );
            summary.skipped += 1;
        }
        SubmitOpResponse::ConflictCopy { .. } => {}
    }

    Ok(())
}

struct ExistingFile<'a> {
    abs_path: PathBuf,
    metadata_len: u64,
    folder_id: &'a str,
    node: LocalNode,
    db: &'a Arc<Mutex<Connection>>,
    client: &'a reqwest::Client,
    backend_url: &'a str,
    token: &'a str,
    device_name: &'a str,
}

async fn process_existing_file(summary: &mut PushSummary, file: ExistingFile<'_>) {
    let stored_version = {
        let conn = file.db.lock().await;
        let version_id = file.node.current_version_id.as_deref().unwrap_or("");
        match versions::get_version(&conn, version_id) {
            Ok(version) => version,
            Err(error) => {
                eprintln!(
                    "push_local: failed to read stored version for {}: {error}",
                    file.abs_path.display()
                );
                summary.errors += 1;
                return;
            }
        }
    };

    if stored_version
        .as_ref()
        .is_some_and(|version| version.size_bytes == file.metadata_len)
    {
        summary.skipped += 1;
        return;
    }

    let date = today_date_str();
    let conn = file.db.lock().await;
    match upload_then_submit_new_version(
        file.client,
        file.backend_url,
        file.token,
        &conn,
        file.folder_id,
        &file.node.node_id,
        file.node.server_seq,
        &file.abs_path,
        file.device_name,
        &date,
    )
    .await
    {
        Ok(_) => summary.versions_submitted += 1,
        Err(error) => {
            eprintln!(
                "push_local: failed to submit new version for {}: {error}",
                file.abs_path.display()
            );
            summary.errors += 1;
        }
    }
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

fn today_date_str() -> String {
    Local::now().date_naive().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use rusqlite::Connection;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
        time::{timeout, Duration},
    };

    use super::*;
    use crate::persistence::{
        nodes::{get_node, upsert_node},
        schema_sql,
        versions::{upsert_version, LocalVersion},
    };

    #[tokio::test]
    async fn push_local_empty_dir_returns_zero_counts() {
        let dir = tempfile::tempdir().unwrap();
        let db = seeded_db();

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            "token",
            "Test Device",
        )
        .await
        .unwrap();

        assert_eq!(summary.creates_submitted, 0);
        assert_eq!(summary.errors, 0);
    }

    #[tokio::test]
    async fn push_local_submits_create_for_new_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("report.md"), b"hello").unwrap();
        let db = seeded_db();
        let (backend_url, server) = local_push_server(vec![
            MockOp::Applied {
                node_id: "srv-1".into(),
                server_seq: 1,
            },
            MockOp::Applied {
                node_id: "version-1".into(),
                server_seq: 2,
            },
        ])
        .await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "Test Device",
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.creates_submitted, 1);
        assert_eq!(summary.versions_submitted, 1);
        assert_eq!(requests.len(), 2);
        let conn = db.lock().await;
        assert!(get_node(&conn, "srv-1").unwrap().is_some());
    }

    #[tokio::test]
    async fn push_local_processes_nested_dir_before_child_file() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir(&docs).unwrap();
        fs::write(docs.join("notes.md"), b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = local_push_server(vec![
            MockOp::Applied {
                node_id: "docs-node".into(),
                server_seq: 2,
            },
            MockOp::Applied {
                node_id: "notes-node".into(),
                server_seq: 3,
            },
        ])
        .await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "Test Device",
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.creates_submitted, 2);
        assert_eq!(requests[0]["payload"]["name"], "docs");
        assert_eq!(requests[1]["payload"]["name"], "notes.md");
        assert_eq!(requests[1]["payload"]["parent_id"], "docs-node");
        let conn = db.lock().await;
        let notes = get_node(&conn, "notes-node").unwrap().unwrap();
        assert_eq!(notes.parent_id.as_deref(), Some("docs-node"));
    }

    #[tokio::test]
    async fn push_local_skips_file_with_matching_size() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("same.txt"), vec![b'a'; 42]).unwrap();
        let db = seeded_db();
        seed_file_with_version(&db, "file-node", "same.txt", 42).await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            "token",
            "Test Device",
        )
        .await
        .unwrap();

        assert_eq!(summary.versions_submitted, 0);
        assert_eq!(summary.skipped, 1);
    }

    #[tokio::test]
    async fn push_local_submits_new_version_for_size_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("changed.txt"), vec![b'a'; 20]).unwrap();
        let db = seeded_db();
        seed_file_with_version(&db, "file-node", "changed.txt", 10).await;
        let (backend_url, server) = local_push_server(vec![MockOp::Applied {
            node_id: "file-node".into(),
            server_seq: 4,
        }])
        .await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "Test Device",
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.versions_submitted, 1);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op_type"], "new_version");
    }

    #[tokio::test]
    async fn push_local_continues_after_per_entry_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"").unwrap();
        fs::write(dir.path().join("b.txt"), b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = local_push_server(vec![
            MockOp::Applied {
                node_id: "a-node".into(),
                server_seq: 2,
            },
            MockOp::Error,
        ])
        .await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "Test Device",
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.creates_submitted, 1);
    }

    #[tokio::test]
    async fn push_local_returns_err_when_no_root_node() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema_sql()).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let error = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            "http://127.0.0.1:1",
            "token",
            "Test Device",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("no root node"));
    }

    #[tokio::test]
    async fn push_local_submits_version_for_new_nonempty_file_in_same_pass() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), b"hello world").unwrap();
        let db = seeded_db();
        let (backend_url, server) = local_push_server(vec![
            MockOp::Applied {
                node_id: "file-node".into(),
                server_seq: 1,
            },
            MockOp::Applied {
                node_id: "version-node".into(),
                server_seq: 2,
            },
        ])
        .await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "Test Device",
        )
        .await
        .unwrap();
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.creates_submitted, 1);
        assert_eq!(summary.versions_submitted, 1);
        assert_eq!(summary.errors, 0);
    }

    #[tokio::test]
    async fn push_local_does_not_submit_version_for_new_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("empty.txt"), b"").unwrap();
        let db = seeded_db();
        let (backend_url, server) = local_push_server(vec![MockOp::Applied {
            node_id: "file-node".into(),
            server_seq: 1,
        }])
        .await;

        let summary = push_local(
            dir.path(),
            "folder-1",
            None,
            &db,
            &reqwest::Client::new(),
            &backend_url,
            "token",
            "Test Device",
        )
        .await
        .unwrap();
        let requests = timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.creates_submitted, 1);
        assert_eq!(summary.versions_submitted, 0);
        assert_eq!(requests.len(), 1);
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

    async fn seed_file_with_version(
        db: &Arc<Mutex<Connection>>,
        node_id: &str,
        name: &str,
        size_bytes: u64,
    ) {
        let conn = db.lock().await;
        upsert_node(
            &conn,
            &LocalNode {
                node_id: node_id.into(),
                folder_id: "folder-1".into(),
                parent_id: Some("root-node".into()),
                name: name.into(),
                node_type: "file".into(),
                current_version_id: Some("version-1".into()),
                server_seq: 3,
                deleted_at: None,
            },
        )
        .unwrap();
        upsert_version(
            &conn,
            &LocalVersion {
                version_id: "version-1".into(),
                node_id: node_id.into(),
                folder_id: "folder-1".into(),
                content_hash: "hash".into(),
                size_bytes,
                manifest_json: "[]".into(),
            },
        )
        .unwrap();
    }

    enum MockOp {
        Applied { node_id: String, server_seq: i64 },
        Error,
    }

    async fn local_push_server(ops: Vec<MockOp>) -> (String, JoinHandle<Vec<serde_json::Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let server_url = base_url.clone();
        let server = tokio::spawn(async move {
            let mut ops = VecDeque::from(ops);
            let mut requests = Vec::new();
            while !ops.is_empty() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (method, path, body) = read_request(&mut stream).await;
                if method == "POST" && path == "/api/objects/batch" {
                    write_batch_response(&mut stream, &server_url, &body).await;
                } else if method == "PUT" && path.starts_with("/upload/") {
                    write_response(&mut stream, "204 No Content", b"").await;
                } else if method == "POST" && path == "/api/folders/folder-1/ops" {
                    requests.push(serde_json::from_slice(&body).unwrap());
                    match ops.pop_front().unwrap() {
                        MockOp::Applied {
                            node_id,
                            server_seq,
                        } => {
                            let body = serde_json::to_vec(&SubmitOpResponse::Applied {
                                node_id,
                                server_seq,
                            })
                            .unwrap();
                            write_response(&mut stream, "200 OK", &body).await;
                        }
                        MockOp::Error => {
                            write_response(&mut stream, "500 Internal Server Error", b"{}").await;
                        }
                    }
                } else {
                    write_response(&mut stream, "404 Not Found", b"").await;
                }
            }
            requests
        });
        (base_url, server)
    }

    async fn write_batch_response(stream: &mut TcpStream, base_url: &str, body: &[u8]) {
        let request: serde_json::Value = serde_json::from_slice(body).unwrap();
        let objects = request["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|object| {
                let oid = object["oid"].as_str().unwrap();
                let size = object["size"].as_u64().unwrap();
                serde_json::json!({
                    "oid": oid,
                    "size": size,
                    "actions": {
                        "upload": {
                            "href": format!("{base_url}/upload/{oid}"),
                            "header": {}
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        let response = serde_json::to_vec(&serde_json::json!({
            "transfer": "basic",
            "objects": objects
        }))
        .unwrap();
        write_response(stream, "200 OK", &response).await;
    }

    async fn read_request(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
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
        let request_line = header_text.lines().next().unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap().to_owned();
        let path = parts.next().unwrap().to_owned();
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

        (method, path, body)
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
