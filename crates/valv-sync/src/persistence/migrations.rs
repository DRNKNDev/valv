use anyhow::Result;
use rusqlite::Connection;

use super::{add_column_if_missing, schema_sql};

type MigrationFn = fn(&Connection) -> Result<()>;

struct Migration {
    version: i64,
    run: MigrationFn,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        run: migration_1,
    },
    Migration {
        version: 2,
        run: migration_2,
    },
];

pub fn run_migrations(conn: &Connection) -> Result<()> {
    let current_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for migration in MIGRATIONS {
        if migration.version <= current_version {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        (migration.run)(&tx)?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }
    Ok(())
}

fn migration_1(conn: &Connection) -> Result<()> {
    conn.execute_batch(schema_sql())?;
    add_column_if_missing(conn, "mounts", "scope_node_id", "TEXT")?;
    add_column_if_missing(conn, "mounts", "mount_token", "TEXT")?;
    add_column_if_missing(conn, "mounts", "can_write", "INTEGER NOT NULL DEFAULT 1")?;
    add_column_if_missing(conn, "mounts", "name", "TEXT")?;
    Ok(())
}

fn migration_2(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "versions")? {
        conn.execute_batch(schema_sql())?;
    }
    add_column_if_missing(conn, "versions", "content_materialized_at", "TEXT")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_versions_node_materialized
         ON versions(node_id, content_materialized_at);",
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;
    use crate::persistence::open_db;

    fn user_version(conn: &Connection) -> i64 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn fresh_database_runs_migration_1() {
        let file = NamedTempFile::new().unwrap();
        let conn = open_db(file.path()).unwrap();

        assert_eq!(user_version(&conn), 2);
        for column in ["scope_node_id", "mount_token", "can_write", "name"] {
            assert!(mount_column_exists(&conn, column));
        }
        assert_eq!(version_column_count(&conn, "content_materialized_at"), 1);
    }

    #[test]
    fn database_at_current_version_skips_migration_ddl() {
        let file = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(file.path()).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }

        let conn = open_db(file.path()).unwrap();

        assert_eq!(user_version(&conn), 2);
        assert!(!table_exists(&conn, "mounts").unwrap());
    }

    #[test]
    fn pre_migration_database_upgrades_without_duplicate_columns() {
        let file = NamedTempFile::new().unwrap();
        {
            let conn = Connection::open(file.path()).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE mounts (
                    path TEXT PRIMARY KEY,
                    folder_id TEXT NOT NULL UNIQUE,
                    grant_id TEXT,
                    cursor INTEGER NOT NULL DEFAULT 0,
                    scope_node_id TEXT
                );
                CREATE TABLE nodes (
                    node_id TEXT PRIMARY KEY,
                    folder_id TEXT NOT NULL,
                    parent_id TEXT,
                    name TEXT NOT NULL,
                    node_type TEXT NOT NULL CHECK (node_type IN ('file', 'folder')),
                    current_version_id TEXT,
                    server_seq INTEGER NOT NULL,
                    deleted_at TEXT
                );
                CREATE TABLE versions (
                    version_id TEXT PRIMARY KEY,
                    node_id TEXT NOT NULL,
                    folder_id TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    manifest_json TEXT NOT NULL
                );
                CREATE TABLE uploaded_chunks (
                    chunk_hash TEXT PRIMARY KEY,
                    size_bytes INTEGER NOT NULL
                );
                "#,
            )
            .unwrap();
        }

        let conn = open_db(file.path()).unwrap();

        assert_eq!(user_version(&conn), 2);
        for column in ["scope_node_id", "mount_token", "can_write", "name"] {
            assert_eq!(mount_column_count(&conn, column), 1);
        }
        assert_eq!(version_column_count(&conn, "content_materialized_at"), 1);
    }

    fn mount_column_exists(conn: &Connection, column: &str) -> bool {
        mount_column_count(conn, column) == 1
    }

    fn mount_column_count(conn: &Connection, column: &str) -> usize {
        column_count(conn, "mounts", column)
    }

    fn version_column_count(conn: &Connection, column: &str) -> usize {
        column_count(conn, "versions", column)
    }

    fn column_count(conn: &Connection, table: &str, column: &str) -> usize {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|result| result.unwrap())
            .filter(|name| name == column)
            .count()
    }
}
