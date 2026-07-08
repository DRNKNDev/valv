use anyhow::{anyhow, Result};
use reqwest::StatusCode;
use rusqlite::Connection;

use crate::{
    persistence::{apply_op_log_entry, apply_tree_snapshot, mounts, nodes, LocalNode},
    protocol::sync::{
        DeltaPullResponse, FolderTreeResponse, OpLogEntry, PROTOCOL_HEADER, PROTOCOL_VERSION,
    },
    sync_engine::update_required::update_required_from_response,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledNode {
    pub node_id: String,
    pub op_type: String,
    pub is_conflict_copy: bool,
    pub actor_device_id: String,
    pub applied_at: String,
    pub old_name: Option<String>,
    pub old_parent_id: Option<String>,
    pub old_version_id: Option<String>,
    pub new_name: String,
    pub new_parent_id: Option<String>,
    pub new_version_id: Option<String>,
    pub node_type: String,
}

pub async fn pull_delta(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    folder_id: &str,
    conn: &mut Connection,
) -> Result<(i64, Vec<PulledNode>)> {
    let mut cursor = mounts::get_cursor(conn, folder_id)?;
    let mut pulled = Vec::new();

    loop {
        let url = format!(
            "{}/folders/{}/ops?since={}",
            crate::api_base(backend_url),
            folder_id,
            cursor
        );
        let response = client
            .get(url)
            .bearer_auth(token)
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
            .send()
            .await?;
        if response.status() == StatusCode::UPGRADE_REQUIRED {
            return Err(update_required_from_response(response).await);
        }
        if response.status() == StatusCode::GONE {
            return tree_resync(client, backend_url, token, folder_id, conn).await;
        }

        let delta = response
            .error_for_status()?
            .json::<DeltaPullResponse>()
            .await?;
        for op in &delta.ops {
            let pre_op = apply_op_log_entry(conn, op)?;
            pulled.push(build_pulled_node(conn, op, pre_op)?);
        }
        mounts::set_cursor(conn, folder_id, delta.up_to_seq)?;

        if delta.ops.is_empty() || delta.up_to_seq <= cursor {
            return Ok((delta.up_to_seq, pulled));
        }
        cursor = delta.up_to_seq;
    }
}

pub async fn tree_resync(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    folder_id: &str,
    conn: &mut Connection,
) -> Result<(i64, Vec<PulledNode>)> {
    let url = format!(
        "{}/folders/{}/tree",
        crate::api_base(backend_url),
        folder_id
    );
    let response = client
        .get(url)
        .bearer_auth(token)
        .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
        .send()
        .await?;
    if response.status() == StatusCode::UPGRADE_REQUIRED {
        return Err(update_required_from_response(response).await);
    }
    let tree = response
        .error_for_status()?
        .json::<FolderTreeResponse>()
        .await?;
    apply_tree_snapshot(conn, folder_id, &tree)?;
    Ok((tree.up_to_seq, Vec::new()))
}

fn build_pulled_node(
    conn: &Connection,
    op: &OpLogEntry,
    pre_op: Option<LocalNode>,
) -> Result<PulledNode> {
    let post_op = nodes::get_node(conn, &op.node_id)?;
    let state = post_op.as_ref().or(pre_op.as_ref()).ok_or_else(|| {
        anyhow!(
            "op `{}` references unknown node `{}` after apply",
            op.op_type,
            op.node_id
        )
    })?;
    let payload = &op.op_payload;
    let payload_string = |field: &str| {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    };

    let new_name = match op.op_type.as_str() {
        "create" => payload_string("name"),
        "rename" => payload_string("new_name"),
        _ => None,
    }
    .unwrap_or_else(|| state.name.clone());
    let new_parent_id = match op.op_type.as_str() {
        "create" => payload_string("parent_id"),
        "move" => payload_string("new_parent_id"),
        _ => post_op.as_ref().and_then(|node| node.parent_id.clone()),
    };
    let new_version_id = match op.op_type.as_str() {
        "new_version" => payload_string("version_id"),
        _ => post_op
            .as_ref()
            .and_then(|node| node.current_version_id.clone()),
    };
    let node_type = if op.op_type == "create" {
        payload_string("type").unwrap_or_else(|| state.node_type.clone())
    } else {
        state.node_type.clone()
    };

    Ok(PulledNode {
        node_id: op.node_id.clone(),
        op_type: op.op_type.clone(),
        is_conflict_copy: op.op_type == "new_version"
            && payload
                .get("is_conflict_copy")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        actor_device_id: op.actor_device_id.clone(),
        applied_at: op.applied_at.clone(),
        old_name: pre_op.as_ref().map(|node| node.name.clone()),
        old_parent_id: pre_op.as_ref().and_then(|node| node.parent_id.clone()),
        old_version_id: pre_op
            .as_ref()
            .and_then(|node| node.current_version_id.clone()),
        new_name,
        new_parent_id,
        new_version_id,
        node_type,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    use super::*;
    use crate::sync_engine::update_required::is_update_required;

    #[tokio::test]
    async fn http_410_falls_back_to_tree_resync() {
        let saw_tree = Arc::new(AtomicBool::new(false));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let server_saw_tree = saw_tree.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                handle_connection(stream, server_saw_tree.clone()).await;
            }
        });

        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::persistence::schema_sql())
            .unwrap();
        mounts::upsert_mount(&conn, "/sync", "folder-1", None, None, None, true).unwrap();

        let (cursor, pulled) = pull_delta(
            &reqwest::Client::new(),
            &base_url,
            "token",
            "folder-1",
            &mut conn,
        )
        .await
        .unwrap();
        server.await.unwrap();

        assert_eq!(cursor, 123);
        assert!(pulled.is_empty());
        assert!(saw_tree.load(Ordering::Acquire));
        assert_eq!(mounts::get_cursor(&conn, "folder-1").unwrap(), 123);
    }

    #[tokio::test]
    async fn pull_delta_unknown_op_type_is_update_required_and_cursor_unchanged() {
        let (base_url, server) = single_response_server(
            "200 OK",
            br#"{"ops":[{"server_seq":6,"node_id":"n1","op_type":"future_op","op_payload":{},"actor_device_id":"d1","applied_at":"2026-07-08T00:00:00Z"}],"up_to_seq":9}"#,
        )
        .await;
        let mut conn = memory_db_with_mount();
        mounts::set_cursor(&conn, "folder-1", 5).unwrap();

        let error = pull_delta(
            &reqwest::Client::new(),
            &base_url,
            "token",
            "folder-1",
            &mut conn,
        )
        .await
        .unwrap_err();
        server.await.unwrap();

        assert!(is_update_required(&error).is_some());
        assert_eq!(mounts::get_cursor(&conn, "folder-1").unwrap(), 5);
    }

    #[tokio::test]
    async fn pull_delta_426_is_update_required_with_min_protocol() {
        let (base_url, server) = single_response_server(
            "426 Upgrade Required",
            br#"{"error":"protocol_too_old","min_protocol":2,"message":"Update Valv"}"#,
        )
        .await;
        let mut conn = memory_db_with_mount();

        let error = pull_delta(
            &reqwest::Client::new(),
            &base_url,
            "token",
            "folder-1",
            &mut conn,
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        let update_required = is_update_required(&error).unwrap();

        assert_eq!(update_required.min_protocol, Some(2));
    }

    #[tokio::test]
    async fn tree_resync_426_is_update_required_with_min_protocol() {
        let (base_url, server) = single_response_server(
            "426 Upgrade Required",
            br#"{"error":"protocol_too_old","min_protocol":3,"message":"Update Valv"}"#,
        )
        .await;
        let mut conn = memory_db_with_mount();

        let error = tree_resync(
            &reqwest::Client::new(),
            &base_url,
            "token",
            "folder-1",
            &mut conn,
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        let update_required = is_update_required(&error).unwrap();

        assert_eq!(update_required.min_protocol, Some(3));
    }

    fn memory_db_with_mount() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::persistence::schema_sql())
            .unwrap();
        mounts::upsert_mount(&conn, "/sync", "folder-1", None, None, None, true).unwrap();
        conn
    }

    async fn single_response_server(
        status: &'static str,
        body: &'static [u8],
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let path = read_path(&mut stream).await;
            write_response(&mut stream, status, body).await;
            path
        });
        (format!("http://{addr}"), server)
    }

    async fn handle_connection(mut stream: TcpStream, saw_tree: Arc<AtomicBool>) {
        let path = read_path(&mut stream).await;
        if path.starts_with("/api/folders/folder-1/ops") {
            write_response(&mut stream, "410 Gone", b"").await;
        } else if path == "/api/folders/folder-1/tree" {
            saw_tree.store(true, Ordering::Release);
            write_response(&mut stream, "200 OK", br#"{"nodes":[],"up_to_seq":123}"#).await;
        } else {
            write_response(&mut stream, "404 Not Found", b"").await;
        }
    }

    async fn read_path(stream: &mut TcpStream) -> String {
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);
        request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned()
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
