CREATE TABLE IF NOT EXISTS mounts (
    path TEXT PRIMARY KEY,
    folder_id TEXT NOT NULL UNIQUE,
    grant_id TEXT,
    scope_node_id TEXT,
    mount_token TEXT,
    cursor INTEGER NOT NULL DEFAULT 0,
    can_write INTEGER NOT NULL DEFAULT 1,
    name TEXT
);

CREATE TABLE IF NOT EXISTS nodes (
    node_id TEXT PRIMARY KEY,
    folder_id TEXT NOT NULL,
    parent_id TEXT,
    name TEXT NOT NULL,
    node_type TEXT NOT NULL CHECK (node_type IN ('file', 'folder')),
    current_version_id TEXT,
    server_seq INTEGER NOT NULL,
    deleted_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_nodes_folder_parent
ON nodes(folder_id, parent_id, deleted_at);

CREATE INDEX IF NOT EXISTS idx_nodes_folder_seq
ON nodes(folder_id, server_seq);

CREATE TABLE IF NOT EXISTS versions (
    version_id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    manifest_json TEXT NOT NULL,
    content_materialized_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_versions_folder
ON versions(folder_id);

CREATE TABLE IF NOT EXISTS uploaded_chunks (
    chunk_hash TEXT PRIMARY KEY,
    size_bytes INTEGER NOT NULL
);
