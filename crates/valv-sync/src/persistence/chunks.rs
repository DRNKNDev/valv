use anyhow::Result;
use rusqlite::{params, Connection};

pub fn mark_uploaded(conn: &Connection, hash: &str, size_bytes: u64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO uploaded_chunks (chunk_hash, size_bytes) VALUES (?1, ?2)",
        params![hash, size_bytes],
    )?;
    Ok(())
}

pub fn is_uploaded(conn: &Connection, hash: &str) -> Result<bool> {
    let count: u64 = conn.query_row(
        "SELECT COUNT(*) FROM uploaded_chunks WHERE chunk_hash = ?1",
        params![hash],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}
