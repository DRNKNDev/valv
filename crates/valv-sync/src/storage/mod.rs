use std::collections::HashMap;

use anyhow::{anyhow, Result};
use bytes::{Bytes, BytesMut};
use reqwest::header::{HeaderName, HeaderValue};
use rusqlite::Connection;

use crate::{
    chunking::Chunk,
    persistence::chunks::{is_uploaded, mark_uploaded},
    protocol::{
        http::{BatchOperation, BatchRequest, BatchRequestObject, BatchResponse},
        sync::ChunkRef,
    },
};

pub async fn upload_chunks(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    conn: &Connection,
    chunks: &[Chunk],
) -> Result<()> {
    let pending = chunks
        .iter()
        .filter_map(|chunk| match is_uploaded(conn, &chunk.hash) {
            Ok(true) => None,
            Ok(false) => Some(Ok(chunk)),
            Err(err) => Some(Err(err)),
        })
        .collect::<Result<Vec<_>>>()?;

    if pending.is_empty() {
        return Ok(());
    }

    let objects = pending
        .iter()
        .map(|chunk| BatchRequestObject {
            oid: chunk.hash.clone(),
            size: chunk.length,
        })
        .collect();
    let response = post_batch(client, backend_url, token, BatchOperation::Upload, objects).await?;
    let chunks_by_hash = pending
        .into_iter()
        .map(|chunk| (chunk.hash.as_str(), chunk))
        .collect::<HashMap<_, _>>();

    for object in response.objects {
        if let Some(error) = object.error {
            return Err(anyhow!(
                "batch upload error for {}: {}",
                object.oid,
                error.message
            ));
        }
        let Some(actions) = object.actions else {
            continue;
        };
        let Some(upload) = actions.upload else {
            continue;
        };
        let chunk = chunks_by_hash
            .get(object.oid.as_str())
            .ok_or_else(|| anyhow!("batch response referenced unknown oid {}", object.oid))?;
        let mut request = client.put(&upload.href).body(chunk.data.clone());
        for (name, value) in upload.header.unwrap_or_default() {
            request = request.header(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            );
        }
        request.send().await?.error_for_status()?;
        mark_uploaded(conn, &chunk.hash, chunk.length)?;
    }

    Ok(())
}

pub async fn download_chunks(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    chunk_refs: &[ChunkRef],
) -> Result<Bytes> {
    let mut refs = chunk_refs.to_vec();
    refs.sort_by_key(|chunk| chunk.offset);
    let objects = refs
        .iter()
        .map(|chunk| BatchRequestObject {
            oid: chunk.chunk_hash.clone(),
            size: chunk.length,
        })
        .collect();
    let response = post_batch(
        client,
        backend_url,
        token,
        BatchOperation::Download,
        objects,
    )
    .await?;
    let objects_by_hash = response
        .objects
        .into_iter()
        .map(|object| (object.oid.clone(), object))
        .collect::<HashMap<_, _>>();
    let mut bytes = BytesMut::new();

    for chunk_ref in refs {
        let object = objects_by_hash
            .get(&chunk_ref.chunk_hash)
            .ok_or_else(|| anyhow!("batch response missing oid {}", chunk_ref.chunk_hash))?;
        if let Some(error) = &object.error {
            return Err(anyhow!(
                "batch download error for {}: {}",
                object.oid,
                error.message
            ));
        }
        let href = object
            .actions
            .as_ref()
            .and_then(|actions| actions.download.as_ref())
            .map(|action| action.href.as_str())
            .ok_or_else(|| anyhow!("batch response missing download action for {}", object.oid))?;
        let chunk_bytes = client
            .get(href)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        bytes.extend_from_slice(&chunk_bytes);
    }

    Ok(bytes.freeze())
}

async fn post_batch(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    operation: BatchOperation,
    objects: Vec<BatchRequestObject>,
) -> Result<BatchResponse> {
    let url = format!("{}/objects/batch", backend_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(&BatchRequest::new(operation, objects))
        .send()
        .await?
        .error_for_status()?;
    let batch = response.json::<BatchResponse>().await?;
    if batch.transfer != "basic" {
        return Err(anyhow!("unsupported batch transfer `{}`", batch.transfer));
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Mutex,
    };

    use super::*;

    #[tokio::test]
    async fn upload_skips_dedup_sends_basic_and_forwards_headers() {
        let state = Arc::new(Mutex::new(MockState::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let server_state = state.clone();
        let server_url = base_url.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                handle_connection(stream, server_state.clone(), server_url.clone()).await;
            }
        });

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::persistence::schema_sql())
            .unwrap();
        let chunks = vec![
            Chunk {
                hash: "dedup".into(),
                offset: 0,
                length: 5,
                data: Bytes::from_static(b"dedup"),
            },
            Chunk {
                hash: "new".into(),
                offset: 5,
                length: 3,
                data: Bytes::from_static(b"new"),
            },
        ];

        upload_chunks(&reqwest::Client::new(), &base_url, "token", &conn, &chunks)
            .await
            .unwrap();
        server.await.unwrap();

        let state = state.lock().await;
        assert_eq!(state.batch_transfer.as_deref(), Some("basic"));
        assert_eq!(state.put_count, 1);
        assert_eq!(state.put_body, b"new");
        assert_eq!(
            state.put_headers.get("content-type").map(String::as_str),
            Some("application/octet-stream")
        );
        assert!(crate::persistence::chunks::is_uploaded(&conn, "new").unwrap());
        assert!(!crate::persistence::chunks::is_uploaded(&conn, "dedup").unwrap());
    }

    #[derive(Default)]
    struct MockState {
        batch_transfer: Option<String>,
        put_count: usize,
        put_headers: HashMap<String, String>,
        put_body: Vec<u8>,
    }

    async fn handle_connection(
        mut stream: TcpStream,
        state: Arc<Mutex<MockState>>,
        base_url: String,
    ) {
        let (method, path, headers, body) = read_request(&mut stream).await;
        if method == "POST" && path == "/objects/batch" {
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            state.lock().await.batch_transfer = json["transfer"].as_str().map(str::to_owned);
            let response = format!(
                r#"{{"transfer":"basic","objects":[{{"oid":"dedup","size":5}},{{"oid":"new","size":3,"actions":{{"upload":{{"href":"{base_url}/upload/new","header":{{"Content-Type":"application/octet-stream"}}}}}}}}]}}"#
            );
            write_response(&mut stream, "200 OK", response.as_bytes()).await;
        } else if method == "PUT" && path == "/upload/new" {
            let mut state = state.lock().await;
            state.put_count += 1;
            state.put_headers = headers;
            state.put_body = body;
            write_response(&mut stream, "204 No Content", b"").await;
        } else {
            write_response(&mut stream, "404 Not Found", b"").await;
        }
    }

    async fn read_request(
        stream: &mut TcpStream,
    ) -> (String, String, HashMap<String, String>, Vec<u8>) {
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
        let mut lines = header_text.lines();
        let request_line = lines.next().unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap().to_owned();
        let path = parts.next().unwrap().to_owned();
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0, "connection closed before body");
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(content_length);

        (method, path, headers, body)
    }

    async fn write_response(stream: &mut TcpStream, status: &str, body: &[u8]) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
    }
}
