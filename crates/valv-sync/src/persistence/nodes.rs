use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNode {
    pub node_id: String,
    pub folder_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub node_type: String,
    pub current_version_id: Option<String>,
    pub server_seq: i64,
    pub deleted_at: Option<String>,
    pub pushed_size_bytes: Option<u64>,
    pub pushed_mtime_nanos: Option<i64>,
}

pub fn get_node(conn: &Connection, node_id: &str) -> Result<Option<LocalNode>> {
    conn.query_row(
        "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
         FROM nodes WHERE node_id = ?1",
        params![node_id],
        row_to_node,
    )
    .optional()
    .map_err(Into::into)
}

pub fn get_node_by_parent_and_name(
    conn: &Connection,
    folder_id: &str,
    parent_id: Option<&str>,
    name: &str,
) -> Result<Option<LocalNode>> {
    match parent_id {
        Some(parent_id) => conn.query_row(
            "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
             FROM nodes
             WHERE folder_id = ?1 AND parent_id = ?2 AND name = ?3 AND deleted_at IS NULL",
            params![folder_id, parent_id, name],
            row_to_node,
        ),
        None => conn.query_row(
            "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
             FROM nodes
             WHERE folder_id = ?1 AND parent_id IS NULL AND name = ?2 AND deleted_at IS NULL",
            params![folder_id, name],
            row_to_node,
        ),
    }
    .optional()
    .map_err(Into::into)
}

pub fn get_root_node(conn: &Connection, folder_id: &str) -> Result<Option<LocalNode>> {
    conn.query_row(
        "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
         FROM nodes
         WHERE folder_id = ?1 AND parent_id IS NULL AND deleted_at IS NULL
         ORDER BY server_seq ASC
         LIMIT 1",
        params![folder_id],
        row_to_node,
    )
    .optional()
    .map_err(Into::into)
}

pub fn upsert_node(conn: &Connection, node: &LocalNode) -> Result<()> {
    conn.execute(
        "INSERT INTO nodes (node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(node_id) DO UPDATE SET
            folder_id = excluded.folder_id,
            parent_id = excluded.parent_id,
            name = excluded.name,
            node_type = excluded.node_type,
            current_version_id = excluded.current_version_id,
            server_seq = excluded.server_seq,
            deleted_at = excluded.deleted_at",
        params![
            node.node_id,
            node.folder_id,
            node.parent_id,
            node.name,
            node.node_type,
            node.current_version_id,
            node.server_seq,
            node.deleted_at,
        ],
    )?;
    Ok(())
}

pub fn mark_deleted(conn: &Connection, node_id: &str, server_seq: i64) -> Result<()> {
    conn.execute(
        "UPDATE nodes SET deleted_at = ?1, server_seq = ?2 WHERE node_id = ?3",
        params![Utc::now().to_rfc3339(), server_seq, node_id],
    )?;
    Ok(())
}

pub fn list_children(
    conn: &Connection,
    parent: Option<&str>,
    folder_id: &str,
    offset: u64,
    limit: u64,
) -> Result<(Vec<LocalNode>, u64)> {
    let parent_clause = if parent.is_some() {
        "parent_id = ?2"
    } else {
        "parent_id IS NULL"
    };
    let count_sql = format!(
        "SELECT COUNT(*) FROM nodes WHERE folder_id = ?1 AND {parent_clause} AND deleted_at IS NULL"
    );
    let list_sql = match parent {
        Some(_) => format!(
            "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
             FROM nodes WHERE folder_id = ?1 AND {parent_clause} AND deleted_at IS NULL
             ORDER BY name ASC LIMIT ?3 OFFSET ?4"
        ),
        None => format!(
            "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
             FROM nodes WHERE folder_id = ?1 AND {parent_clause} AND deleted_at IS NULL
             ORDER BY name ASC LIMIT ?2 OFFSET ?3"
        ),
    };

    let total: u64 = match parent {
        Some(parent) => conn.query_row(&count_sql, params![folder_id, parent], |row| row.get(0))?,
        None => conn.query_row(&count_sql, params![folder_id], |row| row.get(0))?,
    };
    let mut stmt = conn.prepare(&list_sql)?;
    let rows = match parent {
        Some(parent) => stmt.query_map(params![folder_id, parent, limit, offset], row_to_node)?,
        None => stmt.query_map(params![folder_id, limit, offset], row_to_node)?,
    };
    let nodes = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((nodes, total))
}

pub fn list_changed_since(
    conn: &Connection,
    folder_id: &str,
    since_seq: i64,
) -> Result<Vec<LocalNode>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, folder_id, parent_id, name, node_type, current_version_id, server_seq, deleted_at, pushed_size_bytes, pushed_mtime_nanos
         FROM nodes WHERE folder_id = ?1 AND server_seq > ?2 ORDER BY server_seq ASC",
    )?;
    let nodes = stmt
        .query_map(params![folder_id, since_seq], row_to_node)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(nodes)
}

pub fn update_pushed_cache(
    conn: &Connection,
    node_id: &str,
    size_bytes: u64,
    mtime_nanos: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE nodes SET pushed_size_bytes = ?1, pushed_mtime_nanos = ?2 WHERE node_id = ?3",
        params![size_bytes, mtime_nanos, node_id],
    )?;
    Ok(())
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalNode> {
    Ok(LocalNode {
        node_id: row.get(0)?,
        folder_id: row.get(1)?,
        parent_id: row.get(2)?,
        name: row.get(3)?,
        node_type: row.get(4)?,
        current_version_id: row.get(5)?,
        server_seq: row.get(6)?,
        deleted_at: row.get(7)?,
        pushed_size_bytes: row.get(8)?,
        pushed_mtime_nanos: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        conn
    }

    fn file_node(node_id: &str) -> LocalNode {
        LocalNode {
            node_id: node_id.into(),
            folder_id: "folder-1".into(),
            parent_id: None,
            name: "a.txt".into(),
            node_type: "file".into(),
            current_version_id: Some("v1".into()),
            server_seq: 1,
            deleted_at: None,
            pushed_size_bytes: None,
            pushed_mtime_nanos: None,
        }
    }

    #[test]
    fn fresh_insert_has_null_push_cache() {
        let conn = memory_db();
        upsert_node(&conn, &file_node("n1")).unwrap();

        let node = get_node(&conn, "n1").unwrap().unwrap();
        assert_eq!(node.pushed_size_bytes, None);
        assert_eq!(node.pushed_mtime_nanos, None);
    }

    #[test]
    fn rename_triggered_upsert_leaves_push_cache_unchanged() {
        let conn = memory_db();
        upsert_node(&conn, &file_node("n1")).unwrap();
        update_pushed_cache(&conn, "n1", 42, 1_700_000_000_000_000_000).unwrap();

        let mut renamed = file_node("n1");
        renamed.name = "b.txt".into();
        renamed.server_seq = 2;
        upsert_node(&conn, &renamed).unwrap();

        let node = get_node(&conn, "n1").unwrap().unwrap();
        assert_eq!(node.name, "b.txt");
        assert_eq!(node.server_seq, 2);
        assert_eq!(node.pushed_size_bytes, Some(42));
        assert_eq!(node.pushed_mtime_nanos, Some(1_700_000_000_000_000_000));
    }

    #[test]
    fn update_pushed_cache_changes_only_cache_columns() {
        let conn = memory_db();
        upsert_node(&conn, &file_node("n1")).unwrap();

        update_pushed_cache(&conn, "n1", 7, 123).unwrap();

        let node = get_node(&conn, "n1").unwrap().unwrap();
        assert_eq!(node.pushed_size_bytes, Some(7));
        assert_eq!(node.pushed_mtime_nanos, Some(123));
        assert_eq!(node.name, "a.txt");
        assert_eq!(node.parent_id, None);
        assert_eq!(node.current_version_id.as_deref(), Some("v1"));
        assert_eq!(node.server_seq, 1);
        assert_eq!(node.deleted_at, None);
    }
}
