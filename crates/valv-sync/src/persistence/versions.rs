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

pub fn has_any_version_for_node(conn: &Connection, node_id: &str) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM versions WHERE node_id = ?1)",
        params![node_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists != 0)
}

pub fn has_materialized_content_for_node(conn: &Connection, node_id: &str) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM versions
            WHERE node_id = ?1 AND content_materialized_at IS NOT NULL
        )",
        params![node_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists != 0)
}

pub fn has_materialized_content_for_version(conn: &Connection, version_id: &str) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM versions
            WHERE version_id = ?1 AND content_materialized_at IS NOT NULL
        )",
        params![version_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(exists != 0)
}

pub fn mark_content_materialized(conn: &Connection, version_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE versions
         SET content_materialized_at = COALESCE(
            content_materialized_at,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )
         WHERE version_id = ?1",
        params![version_id],
    )?;
    Ok(())
}
