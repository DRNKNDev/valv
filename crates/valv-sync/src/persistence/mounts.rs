use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMount {
    pub path: String,
    pub folder_id: String,
    pub grant_id: Option<String>,
    pub scope_node_id: Option<String>,
    pub mount_token: Option<String>,
    pub cursor: i64,
    pub can_write: bool,
    pub name: Option<String>,
}

pub fn list_mounts(conn: &Connection) -> Result<Vec<LocalMount>> {
    let mut stmt = conn.prepare(
        "SELECT path, folder_id, grant_id, scope_node_id, mount_token, cursor, can_write, name FROM mounts ORDER BY path ASC",
    )?;
    let mounts = stmt
        .query_map([], row_to_mount)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(mounts)
}

pub fn get_mount(conn: &Connection, path: &str) -> Result<Option<LocalMount>> {
    conn.query_row(
        "SELECT path, folder_id, grant_id, scope_node_id, mount_token, cursor, can_write, name FROM mounts WHERE path = ?1",
        params![path],
        row_to_mount,
    )
    .optional()
    .map_err(Into::into)
}

pub fn upsert_mount(
    conn: &Connection,
    path: &str,
    folder_id: &str,
    grant_id: Option<&str>,
    scope_node_id: Option<&str>,
    mount_token: Option<&str>,
    can_write: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO mounts (path, folder_id, grant_id, scope_node_id, mount_token, can_write)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET
            folder_id = excluded.folder_id,
            grant_id = excluded.grant_id,
            scope_node_id = excluded.scope_node_id,
            mount_token = excluded.mount_token,
            can_write = excluded.can_write",
        params![
            path,
            folder_id,
            grant_id,
            scope_node_id,
            mount_token,
            can_write
        ],
    )?;
    Ok(())
}

pub fn set_mount_name(conn: &Connection, path: &str, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE mounts SET name = ?1 WHERE path = ?2",
        params![name, path],
    )?;
    Ok(())
}

pub fn delete_mount(conn: &Connection, path: &str) -> Result<()> {
    conn.execute("DELETE FROM mounts WHERE path = ?1", params![path])?;
    Ok(())
}

pub fn get_cursor(conn: &Connection, folder_id: &str) -> Result<i64> {
    let cursor = conn
        .query_row(
            "SELECT cursor FROM mounts WHERE folder_id = ?1",
            params![folder_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(cursor)
}

pub fn set_cursor(conn: &Connection, folder_id: &str, seq: i64) -> Result<()> {
    conn.execute(
        "UPDATE mounts SET cursor = ?1 WHERE folder_id = ?2",
        params![seq, folder_id],
    )?;
    Ok(())
}

fn row_to_mount(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalMount> {
    Ok(LocalMount {
        path: row.get(0)?,
        folder_id: row.get(1)?,
        grant_id: row.get(2)?,
        scope_node_id: row.get(3)?,
        mount_token: row.get(4)?,
        cursor: row.get(5)?,
        can_write: row.get(6)?,
        name: row.get(7)?,
    })
}
