use std::path::{Component, Path, PathBuf};

use rusqlite::Connection;
use valv_sync::persistence::nodes;

use crate::MountState;

#[derive(Debug)]
pub(crate) struct ResolvedPath {
    pub(crate) mount: MountState,
    pub(crate) folder_id: String,
    pub(crate) node_id: String,
}

pub(crate) enum PathResolutionError {
    NotInMount,
    NodeNotSynced,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for PathResolutionError {
    fn from(error: anyhow::Error) -> Self {
        PathResolutionError::Internal(error)
    }
}

pub(crate) fn normalize_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf())
}

pub(crate) fn resolve_path_to_node(
    conn: &Connection,
    mounts: &[MountState],
    path: &str,
) -> Result<ResolvedPath, PathResolutionError> {
    let local_path = normalize_path(path);
    let (mount, relative_path) = mounts
        .iter()
        .filter_map(|mount| {
            let mount_path = normalize_path(&mount.path);
            local_path.strip_prefix(&mount_path).ok().map(|relative| {
                (
                    mount.clone(),
                    relative.to_path_buf(),
                    mount_path.components().count(),
                )
            })
        })
        .max_by_key(|(_, _, component_count)| *component_count)
        .map(|(mount, relative, _)| (mount, relative))
        .ok_or(PathResolutionError::NotInMount)?;

    let mut current = match mount.scope_node_id.as_deref() {
        Some(scope_node_id) => nodes::get_node(conn, scope_node_id)?,
        None => nodes::get_root_node(conn, &mount.folder_id)?,
    }
    .ok_or(PathResolutionError::NodeNotSynced)?;

    for component in relative_path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_str().ok_or(PathResolutionError::NodeNotSynced)?;
        current = nodes::get_node_by_parent_and_name(
            conn,
            &mount.folder_id,
            Some(&current.node_id),
            name,
        )?
        .ok_or(PathResolutionError::NodeNotSynced)?;
    }

    Ok(ResolvedPath {
        folder_id: mount.folder_id.clone(),
        node_id: current.node_id,
        mount,
    })
}
