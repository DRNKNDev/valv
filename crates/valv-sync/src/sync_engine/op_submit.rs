use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    chunking::chunk_file,
    persistence::versions::{self, LocalVersion},
    protocol::sync::{ChunkRef, NewVersionPayload, SubmitOpRequest, SubmitOpResponse},
    storage::upload_chunks,
};

pub async fn submit_op(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    folder_id: &str,
    req: &SubmitOpRequest,
) -> Result<SubmitOpResponse> {
    let url = format!("{}/folders/{}/ops", crate::api_base(backend_url), folder_id);
    let response = client.post(url).bearer_auth(token).json(req).send().await?;
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow!(
            "authorization failed submitting op for folder {folder_id}"
        ));
    }
    Ok(response
        .error_for_status()?
        .json::<SubmitOpResponse>()
        .await?)
}

pub fn materialize_conflict_copy(
    original_path: &Path,
    device_name: &str,
    date: &str,
) -> Result<PathBuf> {
    let file_name = original_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("path has no valid file name: {}", original_path.display()))?;
    let conflict_name = match original_path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => {
            let stem = original_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    anyhow!("path has no valid file stem: {}", original_path.display())
                })?;
            format!("{stem} (conflicted copy, {device_name}, {date}).{ext}")
        }
        None => format!("{file_name} (conflicted copy, {device_name}, {date})"),
    };
    let conflict_path = original_path.with_file_name(conflict_name);
    fs::copy(original_path, &conflict_path)?;
    Ok(conflict_path)
}

pub async fn upload_then_submit_new_version(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    conn: &Connection,
    folder_id: &str,
    node_id: &str,
    based_on_seq: i64,
    path: &Path,
    device_name: &str,
    date: &str,
) -> Result<SubmitOpResponse> {
    let chunks = chunk_file(path)?;
    upload_chunks(client, backend_url, token, conn, &chunks).await?;
    let manifest = chunks
        .iter()
        .map(|chunk| ChunkRef {
            chunk_hash: chunk.hash.clone(),
            offset: chunk.offset,
            length: chunk.length,
        })
        .collect::<Vec<_>>();
    let req = SubmitOpRequest::NewVersion {
        node_id: node_id.into(),
        based_on_seq,
        payload: NewVersionPayload {
            version_id: Uuid::new_v4().to_string(),
            content_hash: manifest_content_hash(&manifest),
            size_bytes: chunks.iter().map(|chunk| chunk.length).sum(),
            manifest,
        },
    };
    let response = submit_op(client, backend_url, token, folder_id, &req).await?;
    apply_submitted_new_version(conn, folder_id, node_id, &req, &response)?;
    if matches!(response, SubmitOpResponse::ConflictCopy { .. }) {
        materialize_conflict_copy(path, device_name, date)?;
    }
    Ok(response)
}

pub fn apply_submitted_new_version(
    conn: &Connection,
    folder_id: &str,
    node_id: &str,
    req: &SubmitOpRequest,
    response: &SubmitOpResponse,
) -> Result<()> {
    let SubmitOpRequest::NewVersion { payload, .. } = req else {
        return Ok(());
    };
    let (version_id, server_seq, is_conflict_copy) = match response {
        SubmitOpResponse::Applied { server_seq, .. } => (&payload.version_id, *server_seq, false),
        SubmitOpResponse::ConflictCopy {
            server_seq,
            conflict_version_id,
            ..
        } => (conflict_version_id, *server_seq, true),
        SubmitOpResponse::Superseded { .. } => return Ok(()),
    };

    versions::upsert_version(
        conn,
        &LocalVersion {
            version_id: version_id.clone(),
            node_id: node_id.to_owned(),
            folder_id: folder_id.to_owned(),
            content_hash: payload.content_hash.clone(),
            size_bytes: payload.size_bytes,
            manifest_json: serde_json::to_string(&payload.manifest)?,
        },
    )?;
    if is_conflict_copy {
        conn.execute(
            "UPDATE nodes SET server_seq = ?1 WHERE node_id = ?2",
            params![server_seq, node_id],
        )?;
    } else {
        conn.execute(
            "UPDATE nodes SET current_version_id = ?1, server_seq = ?2 WHERE node_id = ?3",
            params![version_id, server_seq, node_id],
        )?;
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn conflict_copy_name_with_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        fs::write(&path, b"content").unwrap();

        let conflict = materialize_conflict_copy(&path, "Alice MacBook", "2026-06-27").unwrap();

        assert_eq!(
            conflict.file_name().and_then(|name| name.to_str()),
            Some("report (conflicted copy, Alice MacBook, 2026-06-27).md")
        );
        assert_eq!(fs::read(conflict).unwrap(), b"content");
    }

    #[test]
    fn conflict_copy_name_without_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Makefile");
        fs::write(&path, b"content").unwrap();

        let conflict = materialize_conflict_copy(&path, "CI Agent", "2026-06-27").unwrap();

        assert_eq!(
            conflict.file_name().and_then(|name| name.to_str()),
            Some("Makefile (conflicted copy, CI Agent, 2026-06-27)")
        );
    }

    #[test]
    fn conflict_copy_name_with_double_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.tar.gz");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"content").unwrap();

        let conflict = materialize_conflict_copy(&path, "CI Agent", "2026-06-27").unwrap();

        assert_eq!(
            conflict.file_name().and_then(|name| name.to_str()),
            Some("archive.tar (conflicted copy, CI Agent, 2026-06-27).gz")
        );
    }
}
