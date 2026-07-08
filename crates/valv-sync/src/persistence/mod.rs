use std::{fs, path::Path};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;

use crate::protocol::sync::{FolderTreeResponse, NodeSnapshot, NodeType, OpLogEntry};
use crate::sync_engine::update_required::UpdateRequired;

pub mod chunks;
pub mod mounts;
pub mod nodes;
pub mod versions;

pub use mounts::LocalMount;
pub use nodes::LocalNode;
pub use versions::LocalVersion;

pub(crate) fn schema_sql() -> &'static str {
    include_str!("schema.sql")
}

pub fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create db directory {}", parent.display()))?;
    }

    let conn =
        Connection::open(path).with_context(|| format!("open sqlite db {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(schema_sql())?;
    Ok(conn)
}

pub fn apply_op_log_entry(conn: &Connection, entry: &OpLogEntry) -> Result<Option<LocalNode>> {
    let pre_op = nodes::get_node(conn, &entry.node_id)?;
    if entry.op_type != "create"
        && pre_op
            .as_ref()
            .is_some_and(|node| node.server_seq >= entry.server_seq)
    {
        return Ok(pre_op);
    }
    match entry.op_type.as_str() {
        "create" => apply_create(conn, entry)?,
        "rename" => {
            let payload: RenamePayload = serde_json::from_value(entry.op_payload.clone())?;
            conn.execute(
                "UPDATE nodes SET name = ?1, server_seq = ?2 WHERE node_id = ?3",
                params![payload.new_name, entry.server_seq, entry.node_id],
            )?;
        }
        "move" => {
            let payload: MovePayload = serde_json::from_value(entry.op_payload.clone())?;
            conn.execute(
                "UPDATE nodes SET parent_id = ?1, server_seq = ?2 WHERE node_id = ?3",
                params![payload.new_parent_id, entry.server_seq, entry.node_id],
            )?;
        }
        "delete" => {
            let deleted_at = Utc::now().to_rfc3339();
            conn.execute(
                "UPDATE nodes SET deleted_at = ?1, server_seq = ?2 WHERE node_id = ?3",
                params![deleted_at, entry.server_seq, entry.node_id],
            )?;
        }
        "new_version" => apply_new_version(conn, entry)?,
        other => return Err(anyhow!(UpdateRequired::unrecognized_op_type(other))),
    };
    Ok(pre_op)
}

pub fn apply_tree_snapshot(
    conn: &mut Connection,
    folder_id: &str,
    resp: &FolderTreeResponse,
) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM versions WHERE folder_id = ?1",
        params![folder_id],
    )?;
    tx.execute("DELETE FROM nodes WHERE folder_id = ?1", params![folder_id])?;

    for node in &resp.nodes {
        insert_snapshot_node(&tx, folder_id, node)?;
    }

    tx.execute(
        "UPDATE mounts SET cursor = ?1 WHERE folder_id = ?2",
        params![resp.up_to_seq, folder_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn apply_create(conn: &Connection, entry: &OpLogEntry) -> Result<()> {
    let payload: CreatePayload = serde_json::from_value(entry.op_payload.clone())?;
    let folder_id = match payload.folder_id {
        Some(folder_id) => folder_id,
        None => folder_id_from_parent(conn, &payload.parent_id)?.ok_or_else(|| {
            anyhow!(
                "create op `{}` is missing folder_id and parent `{}` is not in the local mirror",
                entry.node_id,
                payload.parent_id
            )
        })?,
    };

    nodes::upsert_node(
        conn,
        &nodes::LocalNode {
            node_id: entry.node_id.clone(),
            folder_id,
            parent_id: Some(payload.parent_id),
            name: payload.name,
            node_type: node_type_to_str(&payload.node_type).into(),
            current_version_id: None,
            server_seq: entry.server_seq,
            deleted_at: None,
        },
    )
}

fn apply_new_version(conn: &Connection, entry: &OpLogEntry) -> Result<()> {
    let payload: NewVersionPayload = serde_json::from_value(entry.op_payload.clone())?;
    let folder_id = nodes::get_node(conn, &entry.node_id)?
        .map(|node| node.folder_id)
        .ok_or_else(|| anyhow!("new_version op references unknown node `{}`", entry.node_id))?;
    let manifest_json = serde_json::to_string(&payload.manifest)?;

    versions::upsert_version(
        conn,
        &versions::LocalVersion {
            version_id: payload.version_id.clone(),
            node_id: entry.node_id.clone(),
            folder_id,
            content_hash: payload.content_hash,
            size_bytes: payload.size_bytes,
            manifest_json,
        },
    )?;
    if payload.is_conflict_copy != Some(true) {
        conn.execute(
            "UPDATE nodes SET current_version_id = ?1, server_seq = ?2 WHERE node_id = ?3",
            params![payload.version_id, entry.server_seq, entry.node_id],
        )?;
    } else {
        conn.execute(
            "UPDATE nodes SET server_seq = ?1 WHERE node_id = ?2",
            params![entry.server_seq, entry.node_id],
        )?;
    }
    Ok(())
}

fn folder_id_from_parent(conn: &Connection, parent_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT folder_id FROM nodes WHERE node_id = ?1",
        params![parent_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn insert_snapshot_node(conn: &Connection, folder_id: &str, node: &NodeSnapshot) -> Result<()> {
    nodes::upsert_node(
        conn,
        &nodes::LocalNode {
            node_id: node.node_id.clone(),
            folder_id: folder_id.into(),
            parent_id: node.parent_id.clone(),
            name: node.name.clone(),
            node_type: node_type_to_str(&node.node_type).into(),
            current_version_id: node.current_version_id.clone(),
            server_seq: node.server_seq,
            deleted_at: node.deleted_at.clone(),
        },
    )
}

fn node_type_to_str(node_type: &NodeType) -> &'static str {
    match node_type {
        NodeType::File => "file",
        NodeType::Folder => "folder",
    }
}

#[derive(Deserialize)]
struct CreatePayload {
    parent_id: String,
    name: String,
    #[serde(rename = "type")]
    node_type: NodeType,
    folder_id: Option<String>,
}

#[derive(Deserialize)]
struct RenamePayload {
    new_name: String,
}

#[derive(Deserialize)]
struct MovePayload {
    new_parent_id: String,
}

#[derive(Deserialize)]
struct NewVersionPayload {
    version_id: String,
    content_hash: String,
    size_bytes: u64,
    manifest: Vec<crate::protocol::sync::ChunkRef>,
    #[serde(default)]
    is_conflict_copy: Option<bool>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::protocol::sync::{FolderTreeResponse, NodeSnapshot};
    use crate::sync_engine::update_required::is_update_required;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        conn
    }

    #[test]
    fn fresh_database_has_full_mounts_schema() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let conn = open_db(file.path()).unwrap();

        let mut stmt = conn.prepare("PRAGMA table_info(mounts)").unwrap();
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|result| result.unwrap())
            .collect();

        for column in [
            "path",
            "folder_id",
            "grant_id",
            "scope_node_id",
            "mount_token",
            "cursor",
            "can_write",
            "name",
        ] {
            assert!(
                columns.iter().any(|name| name == column),
                "expected mounts.{column} to exist, got columns {columns:?}"
            );
        }
    }

    #[test]
    fn apply_op_log_sequence_updates_mirror_state() {
        let conn = memory_db();
        mounts::upsert_mount(&conn, "/sync", "folder-1", None, None, None, true).unwrap();

        let entries = vec![
            OpLogEntry {
                server_seq: 1,
                node_id: "n1".into(),
                op_type: "create".into(),
                op_payload: json!({"node_id":"n1","folder_id":"folder-1","parent_id":"root","name":"a.txt","type":"file"}),
                actor_device_id: "d1".into(),
                applied_at: "2026-06-28T00:00:00Z".into(),
            },
            OpLogEntry {
                server_seq: 2,
                node_id: "n1".into(),
                op_type: "rename".into(),
                op_payload: json!({"new_name":"b.txt"}),
                actor_device_id: "d1".into(),
                applied_at: "2026-06-28T00:00:01Z".into(),
            },
            OpLogEntry {
                server_seq: 3,
                node_id: "n1".into(),
                op_type: "move".into(),
                op_payload: json!({"new_parent_id":"folder-a"}),
                actor_device_id: "d1".into(),
                applied_at: "2026-06-28T00:00:02Z".into(),
            },
            OpLogEntry {
                server_seq: 4,
                node_id: "n1".into(),
                op_type: "new_version".into(),
                op_payload: json!({"version_id":"v1","content_hash":"hash","size_bytes":3,"manifest":[{"chunk_hash":"c1","offset":0,"length":3}],"is_conflict_copy":null}),
                actor_device_id: "d1".into(),
                applied_at: "2026-06-28T00:00:03Z".into(),
            },
            OpLogEntry {
                server_seq: 5,
                node_id: "n1".into(),
                op_type: "delete".into(),
                op_payload: json!({}),
                actor_device_id: "d1".into(),
                applied_at: "2026-06-28T00:00:04Z".into(),
            },
        ];

        for entry in &entries {
            apply_op_log_entry(&conn, entry).unwrap();
        }

        let node = nodes::get_node(&conn, "n1").unwrap().unwrap();
        assert_eq!(node.name, "b.txt");
        assert_eq!(node.parent_id.as_deref(), Some("folder-a"));
        assert_eq!(node.current_version_id.as_deref(), Some("v1"));
        assert_eq!(node.server_seq, 5);
        assert!(node.deleted_at.is_some());
        assert!(versions::get_version(&conn, "v1").unwrap().is_some());
        assert!(!versions::has_materialized_content_for_node(&conn, "n1").unwrap());
    }

    #[test]
    fn apply_new_version_conflict_copy_preserves_current_version() {
        let conn = memory_db();
        nodes::upsert_node(
            &conn,
            &nodes::LocalNode {
                node_id: "n1".into(),
                folder_id: "folder-1".into(),
                parent_id: None,
                name: "shared.txt".into(),
                node_type: "file".into(),
                current_version_id: Some("winner".into()),
                server_seq: 10,
                deleted_at: None,
            },
        )
        .unwrap();

        apply_new_version(
            &conn,
            &OpLogEntry {
                server_seq: 11,
                node_id: "n1".into(),
                op_type: "new_version".into(),
                op_payload: json!({"version_id":"loser","content_hash":"hash","size_bytes":3,"manifest":[{"chunk_hash":"c1","offset":0,"length":3}],"is_conflict_copy":true}),
                actor_device_id: "d2".into(),
                applied_at: "2026-06-28T00:00:03Z".into(),
            },
        )
        .unwrap();

        let node = nodes::get_node(&conn, "n1").unwrap().unwrap();
        assert_eq!(node.current_version_id.as_deref(), Some("winner"));
        assert_eq!(node.server_seq, 11);
        assert!(versions::get_version(&conn, "loser").unwrap().is_some());
    }

    #[test]
    fn unrecognized_op_type_returns_update_required() {
        let conn = memory_db();
        let error = apply_op_log_entry(
            &conn,
            &OpLogEntry {
                server_seq: 1,
                node_id: "n1".into(),
                op_type: "future_op".into(),
                op_payload: json!({}),
                actor_device_id: "d1".into(),
                applied_at: "2026-07-08T00:00:00Z".into(),
            },
        )
        .unwrap_err();

        assert!(is_update_required(&error).is_some());
    }

    #[test]
    fn stale_unrecognized_op_type_is_noop_not_update_required() {
        let conn = memory_db();
        nodes::upsert_node(
            &conn,
            &nodes::LocalNode {
                node_id: "n1".into(),
                folder_id: "folder-1".into(),
                parent_id: None,
                name: "current.txt".into(),
                node_type: "file".into(),
                current_version_id: None,
                server_seq: 10,
                deleted_at: None,
            },
        )
        .unwrap();

        let pre_op = apply_op_log_entry(
            &conn,
            &OpLogEntry {
                server_seq: 8,
                node_id: "n1".into(),
                op_type: "future_op".into(),
                op_payload: json!({}),
                actor_device_id: "d1".into(),
                applied_at: "2026-07-08T00:00:00Z".into(),
            },
        )
        .unwrap()
        .unwrap();
        let node = nodes::get_node(&conn, "n1").unwrap().unwrap();

        assert_eq!(pre_op.server_seq, 10);
        assert_eq!(node.server_seq, 10);
        assert_eq!(node.name, "current.txt");
    }

    #[test]
    fn out_of_order_non_create_ops_do_not_mutate_node() {
        for (op_type, payload) in [
            ("rename", json!({"new_name":"stale.txt"})),
            ("move", json!({"new_parent_id":"stale-parent"})),
            ("delete", json!({})),
            (
                "new_version",
                json!({"version_id":"stale-v","content_hash":"hash","size_bytes":3,"manifest":[{"chunk_hash":"c1","offset":0,"length":3}]}),
            ),
        ] {
            let conn = memory_db();
            nodes::upsert_node(
                &conn,
                &nodes::LocalNode {
                    node_id: "n1".into(),
                    folder_id: "folder-1".into(),
                    parent_id: Some("parent".into()),
                    name: "current.txt".into(),
                    node_type: "file".into(),
                    current_version_id: Some("current-v".into()),
                    server_seq: 10,
                    deleted_at: None,
                },
            )
            .unwrap();

            let pre_op = apply_op_log_entry(
                &conn,
                &OpLogEntry {
                    server_seq: 8,
                    node_id: "n1".into(),
                    op_type: op_type.into(),
                    op_payload: payload,
                    actor_device_id: "d1".into(),
                    applied_at: "2026-07-08T00:00:00Z".into(),
                },
            )
            .unwrap()
            .unwrap();
            let node = nodes::get_node(&conn, "n1").unwrap().unwrap();

            assert_eq!(pre_op.server_seq, 10);
            assert_eq!(node.name, "current.txt");
            assert_eq!(node.parent_id.as_deref(), Some("parent"));
            assert_eq!(node.current_version_id.as_deref(), Some("current-v"));
            assert_eq!(node.server_seq, 10);
            assert!(node.deleted_at.is_none());
            assert!(versions::get_version(&conn, "stale-v").unwrap().is_none());
        }
    }

    #[test]
    fn duplicate_server_seq_is_not_reapplied() {
        let conn = memory_db();
        nodes::upsert_node(
            &conn,
            &nodes::LocalNode {
                node_id: "n1".into(),
                folder_id: "folder-1".into(),
                parent_id: None,
                name: "current.txt".into(),
                node_type: "file".into(),
                current_version_id: None,
                server_seq: 10,
                deleted_at: None,
            },
        )
        .unwrap();

        apply_op_log_entry(
            &conn,
            &OpLogEntry {
                server_seq: 10,
                node_id: "n1".into(),
                op_type: "rename".into(),
                op_payload: json!({"new_name":"duplicate.txt"}),
                actor_device_id: "d1".into(),
                applied_at: "2026-07-08T00:00:00Z".into(),
            },
        )
        .unwrap();
        let node = nodes::get_node(&conn, "n1").unwrap().unwrap();

        assert_eq!(node.name, "current.txt");
        assert_eq!(node.server_seq, 10);
    }

    #[test]
    fn create_op_is_exempt_from_monotonicity_guard() {
        let conn = memory_db();

        apply_op_log_entry(
            &conn,
            &OpLogEntry {
                server_seq: 1,
                node_id: "n1".into(),
                op_type: "create".into(),
                op_payload: json!({"node_id":"n1","folder_id":"folder-1","parent_id":"root","name":"created.txt","type":"file"}),
                actor_device_id: "d1".into(),
                applied_at: "2026-07-08T00:00:00Z".into(),
            },
        )
        .unwrap();
        let node = nodes::get_node(&conn, "n1").unwrap().unwrap();

        assert_eq!(node.name, "created.txt");
        assert_eq!(node.server_seq, 1);
    }

    #[test]
    fn apply_tree_snapshot_replaces_stale_rows() {
        let mut conn = memory_db();
        mounts::upsert_mount(&conn, "/sync", "folder-1", None, None, None, true).unwrap();
        nodes::upsert_node(
            &conn,
            &nodes::LocalNode {
                node_id: "stale".into(),
                folder_id: "folder-1".into(),
                parent_id: None,
                name: "stale.txt".into(),
                node_type: "file".into(),
                current_version_id: None,
                server_seq: 1,
                deleted_at: None,
            },
        )
        .unwrap();

        let snapshot = FolderTreeResponse {
            up_to_seq: 100,
            nodes: vec![NodeSnapshot {
                node_id: "fresh".into(),
                parent_id: None,
                name: "fresh.txt".into(),
                node_type: NodeType::File,
                current_version_id: None,
                server_seq: 100,
                deleted_at: None,
            }],
        };

        apply_tree_snapshot(&mut conn, "folder-1", &snapshot).unwrap();

        assert!(nodes::get_node(&conn, "stale").unwrap().is_none());
        assert!(nodes::get_node(&conn, "fresh").unwrap().is_some());
        assert_eq!(mounts::get_cursor(&conn, "folder-1").unwrap(), 100);
    }
}
