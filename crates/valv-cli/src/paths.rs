use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use valv_sync::{
    persistence::{
        mounts::{self, LocalMount},
        nodes, open_db,
    },
    watch::resolve_abs_path,
};

use crate::error::CliError;

#[derive(Debug)]
pub(crate) struct ResolvedTarget {
    pub(crate) folder_id: String,
    pub(crate) scope_node_id: String,
}

fn open_sync_db() -> Result<Connection> {
    let db_path = data_dir()
        .context("failed to determine Valv data directory")?
        .join("sync.db");
    open_db(&db_path).with_context(|| format!("failed to open sync database {}", db_path.display()))
}

fn covering_mount(conn: &Connection, target: &Path) -> Result<LocalMount> {
    mounts::list_mounts(conn)
        .context("failed to list mounted folders")?
        .into_iter()
        .filter(|mount| target.starts_with(&mount.path))
        .max_by_key(|mount| mount.path.len())
        .ok_or_else(|| CliError::path_not_mounted(target.display().to_string()).into())
}

pub(crate) fn resolve_mount(path: &str) -> Result<LocalMount> {
    let conn = open_sync_db()?;
    let target = resolve_target(path);
    covering_mount(&conn, &target)
}

pub(crate) fn list_local_mounts() -> Result<Vec<LocalMount>> {
    let conn = open_sync_db()?;
    mounts::list_mounts(&conn).context("failed to list mounted folders")
}

pub(crate) fn resolve_target_path(path: &str) -> Result<ResolvedTarget> {
    let conn = open_sync_db()?;
    let target = resolve_target(path);
    let mount = covering_mount(&conn, &target)?;
    let node = resolve_abs_path(&conn, Path::new(&mount.path), &mount.folder_id, &target)
        .with_context(|| format!("failed to resolve target path {}", target.display()))?
        .ok_or_else(|| CliError::path_not_in_mirror(target.display().to_string()))?;
    Ok(ResolvedTarget {
        folder_id: mount.folder_id,
        scope_node_id: node.node_id,
    })
}

pub(crate) fn scope_label(folder_id: &str, scope_node_id: &str) -> Result<String> {
    let conn = open_sync_db()?;
    if let Some(root) = nodes::get_root_node(&conn, folder_id)
        .context("failed to look up the folder's root node")?
    {
        if root.node_id == scope_node_id {
            return Ok("Entire Folder".to_owned());
        }
    }
    if let Some(node) =
        nodes::get_node(&conn, scope_node_id).context("failed to look up the scope node")?
    {
        return Ok(node.name);
    }
    Ok(format!(
        "subtree {}",
        &scope_node_id[..scope_node_id.len().min(8)]
    ))
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

fn resolve_target(path: &str) -> PathBuf {
    let expanded = expand_tilde(path);
    std::fs::canonicalize(&expanded).unwrap_or(expanded)
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

#[cfg(target_os = "macos")]
pub(crate) fn daemon_log_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Logs/Valv/valvd.log"))
}

#[cfg(target_os = "linux")]
pub(crate) fn systemd_unit_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".config/systemd/user/valvd.service"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use valv_sync::persistence::LocalNode;

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

    #[test]
    fn scope_label_names_the_root_and_falls_back_to_the_node_name_or_a_truncated_id() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = env::var_os("HOME");
        env::set_var("HOME", dir.path());

        let conn = open_db(&data_dir().unwrap().join("sync.db")).unwrap();
        nodes::upsert_node(
            &conn,
            &LocalNode {
                node_id: "root-1".into(),
                folder_id: "folder-1".into(),
                parent_id: None,
                name: String::new(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 0,
                deleted_at: None,
                pushed_size_bytes: None,
                pushed_mtime_nanos: None,
            },
        )
        .unwrap();
        nodes::upsert_node(
            &conn,
            &LocalNode {
                node_id: "sub-1".into(),
                folder_id: "folder-1".into(),
                parent_id: Some("root-1".into()),
                name: "assets".into(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 1,
                deleted_at: None,
                pushed_size_bytes: None,
                pushed_mtime_nanos: None,
            },
        )
        .unwrap();
        drop(conn);

        let root_label = scope_label("folder-1", "root-1").unwrap();
        let sub_label = scope_label("folder-1", "sub-1").unwrap();
        let unknown_label =
            scope_label("folder-1", "ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();

        match previous_home {
            Some(home) => env::set_var("HOME", home),
            None => env::remove_var("HOME"),
        }

        assert_eq!(root_label, "Entire Folder");
        assert_eq!(sub_label, "assets");
        assert_eq!(unknown_label, "subtree ffffffff");
    }

    fn setup_home_with_mount(home: &Path, mount_path: &Path, folder_id: &str) {
        let conn = open_db(&home.join(".local/share/valv/sync.db")).unwrap();
        mounts::upsert_mount(
            &conn,
            &mount_path.to_string_lossy(),
            folder_id,
            None,
            None,
            None,
            true,
        )
        .unwrap();
    }

    #[test]
    fn resolve_mount_matches_a_canonical_absolute_path() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = env::var_os("HOME");
        env::set_var("HOME", home.path());

        let mount_dir = tempfile::tempdir().unwrap();
        let mount_path = std::fs::canonicalize(mount_dir.path()).unwrap();
        setup_home_with_mount(home.path(), &mount_path, "folder-1");

        let resolved = resolve_mount(&mount_path.to_string_lossy()).unwrap();

        match previous_home {
            Some(home) => env::set_var("HOME", home),
            None => env::remove_var("HOME"),
        }

        assert_eq!(resolved.folder_id, "folder-1");
    }

    #[test]
    fn resolve_mount_resolves_a_dot_relative_path_from_inside_the_mount() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = env::var_os("HOME");
        env::set_var("HOME", home.path());

        let mount_dir = tempfile::tempdir().unwrap();
        let mount_path = std::fs::canonicalize(mount_dir.path()).unwrap();
        setup_home_with_mount(home.path(), &mount_path, "folder-2");

        let previous_cwd = env::current_dir().unwrap();
        env::set_current_dir(&mount_path).unwrap();
        let result = resolve_mount(".");
        env::set_current_dir(previous_cwd).unwrap();

        match previous_home {
            Some(home) => env::set_var("HOME", home),
            None => env::remove_var("HOME"),
        }

        assert_eq!(result.unwrap().folder_id, "folder-2");
    }

    #[test]
    fn resolve_mount_resolves_a_bare_relative_name_from_the_parent_directory() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = env::var_os("HOME");
        env::set_var("HOME", home.path());

        let parent_dir = tempfile::tempdir().unwrap();
        let parent_path = std::fs::canonicalize(parent_dir.path()).unwrap();
        let mount_name = "mydir";
        let mount_path = parent_path.join(mount_name);
        std::fs::create_dir(&mount_path).unwrap();
        setup_home_with_mount(home.path(), &mount_path, "folder-3");

        let previous_cwd = env::current_dir().unwrap();
        env::set_current_dir(&parent_path).unwrap();
        let result = resolve_mount(mount_name);
        env::set_current_dir(previous_cwd).unwrap();

        match previous_home {
            Some(home) => env::set_var("HOME", home),
            None => env::remove_var("HOME"),
        }

        assert_eq!(result.unwrap().folder_id, "folder-3");
    }

    #[test]
    fn resolve_mount_falls_back_to_tilde_expansion_when_the_target_does_not_exist_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = env::var_os("HOME");
        env::set_var("HOME", home.path());

        let mount_dir = tempfile::tempdir().unwrap();
        let mount_path = std::fs::canonicalize(mount_dir.path()).unwrap();
        setup_home_with_mount(home.path(), &mount_path, "folder-4");

        let missing_path = mount_path.join("does-not-exist");
        let result = resolve_mount(&missing_path.to_string_lossy());

        match previous_home {
            Some(home) => env::set_var("HOME", home),
            None => env::remove_var("HOME"),
        }

        assert_eq!(result.unwrap().folder_id, "folder-4");
    }
}
