use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use valv_sync::{
    persistence::{mounts, open_db},
    watch::resolve_abs_path,
};

#[derive(Debug)]
pub(crate) struct ResolvedTarget {
    pub(crate) folder_id: String,
    pub(crate) scope_node_id: String,
}

pub(crate) fn resolve_target_path(path: &str) -> Result<ResolvedTarget> {
    let db_path = data_dir()
        .context("failed to determine Valv data directory")?
        .join("sync.db");
    let conn = open_db(&db_path)
        .with_context(|| format!("failed to open sync database {}", db_path.display()))?;
    let target = expand_tilde(path);
    let mount = mounts::list_mounts(&conn)
        .context("failed to list mounted folders")?
        .into_iter()
        .filter(|mount| target.starts_with(&mount.path))
        .max_by_key(|mount| mount.path.len())
        .ok_or_else(|| anyhow!("path is not inside a mounted folder: {}", target.display()))?;
    let node = resolve_abs_path(&conn, Path::new(&mount.path), &mount.folder_id, &target)
        .with_context(|| format!("failed to resolve target path {}", target.display()))?
        .ok_or_else(|| {
            anyhow!(
                "path is not present in the local mirror: {}",
                target.display()
            )
        })?;
    Ok(ResolvedTarget {
        folder_id: mount.folder_id,
        scope_node_id: node.node_id,
    })
}

pub(crate) fn first_mount_folder_id() -> Result<String> {
    let db_path = data_dir()
        .context("failed to determine Valv data directory")?
        .join("sync.db");
    let conn = open_db(&db_path)
        .with_context(|| format!("failed to open sync database {}", db_path.display()))?;
    let mount = mounts::list_mounts(&conn)
        .context("failed to list mounted folders")?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no mounted folders"))?;
    Ok(mount.folder_id)
}

pub(crate) fn resolve_valvd_path() -> Result<PathBuf> {
    let current = env::current_exe().context("failed to determine current executable path")?;
    if let Some(parent) = current.parent() {
        let sibling = parent.join("valvd");
        if sibling.exists() {
            return Ok(sibling);
        }
    }
    Ok(PathBuf::from("/usr/local/bin/valvd"))
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|_| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(home_dir()
        .context("failed to determine home directory for config path")?
        .join(".config/valv/config.toml"))
}

pub(crate) fn data_dir() -> Result<PathBuf> {
    Ok(home_dir()
        .context("failed to determine home directory for data path")?
        .join(".local/share/valv"))
}

pub(crate) fn socket_path() -> Result<PathBuf> {
    Ok(data_dir()
        .context("failed to determine Valv data directory for socket path")?
        .join("valvd.sock"))
}

pub(crate) fn local_state_dir() -> Result<PathBuf> {
    Ok(home_dir()
        .context("failed to determine home directory for local state path")?
        .join(".local/share/valv"))
}

pub(crate) fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))
}

#[cfg(target_os = "macos")]
pub(crate) fn app_managed_bin_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Application Support/Valv/bin"))
}

#[cfg(target_os = "macos")]
pub(crate) fn launch_agent_plist_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/LaunchAgents/dev.drnkn.valvd.plist"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_path_falls_back_to_valvd_binary_name() {
        assert_eq!(
            resolve_valvd_path()
                .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/valvd"))
                .file_name()
                .and_then(|name| name.to_str()),
            Some("valvd")
        );
    }
}
