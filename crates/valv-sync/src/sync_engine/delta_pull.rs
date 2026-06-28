use anyhow::Result;
use reqwest::StatusCode;
use rusqlite::Connection;

use crate::{
    persistence::{apply_op_log_entry, apply_tree_snapshot, mounts},
    protocol::sync::{DeltaPullResponse, FolderTreeResponse},
};

pub async fn pull_delta(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    folder_id: &str,
    conn: &mut Connection,
) -> Result<i64> {
    let mut cursor = mounts::get_cursor(conn, folder_id)?;

    loop {
        let url = format!(
            "{}/folders/{}/ops?since={}",
            backend_url.trim_end_matches('/'),
            folder_id,
            cursor
        );
        let response = client.get(url).bearer_auth(token).send().await?;
        if response.status() == StatusCode::GONE {
            return tree_resync(client, backend_url, token, folder_id, conn).await;
        }

        let delta = response
            .error_for_status()?
            .json::<DeltaPullResponse>()
            .await?;
        for op in &delta.ops {
            apply_op_log_entry(conn, op)?;
        }
        mounts::set_cursor(conn, folder_id, delta.up_to_seq)?;

        if delta.ops.is_empty() || delta.up_to_seq <= cursor {
            return Ok(delta.up_to_seq);
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
) -> Result<i64> {
    let url = format!(
        "{}/folders/{}/tree",
        backend_url.trim_end_matches('/'),
        folder_id
    );
    let tree = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<FolderTreeResponse>()
        .await?;
    apply_tree_snapshot(conn, folder_id, &tree)?;
    Ok(tree.up_to_seq)
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
        mounts::upsert_mount(&conn, "/sync", "folder-1", None, None, None).unwrap();

        let cursor = pull_delta(
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
        assert!(saw_tree.load(Ordering::Acquire));
        assert_eq!(mounts::get_cursor(&conn, "folder-1").unwrap(), 123);
    }

    async fn handle_connection(mut stream: TcpStream, saw_tree: Arc<AtomicBool>) {
        let path = read_path(&mut stream).await;
        if path.starts_with("/folders/folder-1/ops") {
            write_response(&mut stream, "410 Gone", b"").await;
        } else if path == "/folders/folder-1/tree" {
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
