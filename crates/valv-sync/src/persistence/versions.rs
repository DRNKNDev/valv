use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVersion {
    pub version_id: String,
    pub node_id: String,
    pub folder_id: String,
    pub content_hash: String,
    pub size_bytes: u64,
    pub manifest_json: String,
}

pub fn upsert_version(conn: &Connection, version: &LocalVersion) -> Result<()> {
    conn.execute(
        "INSERT INTO versions (version_id, node_id, folder_id, content_hash, size_bytes, manifest_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(version_id) DO UPDATE SET
            node_id = excluded.node_id,
            folder_id = excluded.folder_id,
            content_hash = excluded.content_hash,
            size_bytes = excluded.size_bytes,
            manifest_json = excluded.manifest_json",
        params![
            version.version_id,
            version.node_id,
            version.folder_id,
            version.content_hash,
            version.size_bytes,
            version.manifest_json,
        ],
    )?;
    Ok(())
}

pub fn get_version(conn: &Connection, version_id: &str) -> Result<Option<LocalVersion>> {
    conn.query_row(
        "SELECT version_id, node_id, folder_id, content_hash, size_bytes, manifest_json
         FROM versions WHERE version_id = ?1",
        params![version_id],
        |row| {
            Ok(LocalVersion {
                version_id: row.get(0)?,
                node_id: row.get(1)?,
                folder_id: row.get(2)?,
                content_hash: row.get(3)?,
                size_bytes: row.get(4)?,
                manifest_json: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}
