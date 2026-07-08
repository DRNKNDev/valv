use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::Deserialize;
use valv_sync::{config::toml_escape, persistence::mounts};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DaemonConfig {
    pub(crate) backend_url: String,
    pub(crate) device_id: String,
    pub(crate) device_token: String,
    pub(crate) device_name: String,
    #[serde(default)]
    pub(crate) mounts: Vec<MountConfig>,
}

#[derive(Debug, Deserialize)]
struct RawDaemonConfig {
    backend_url: Option<String>,
    device_id: Option<String>,
    device_token: Option<String>,
    device_name: Option<String>,
    #[serde(default)]
    mounts: Vec<MountConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MountConfig {
    pub(crate) path: String,
    pub(crate) folder_id: String,
    pub(crate) grant_id: Option<String>,
    pub(crate) scope_node_id: Option<String>,
    pub(crate) mount_token: Option<String>,
}

pub(crate) fn merge_config_mounts(conn: &Connection, config_mounts: &[MountConfig]) -> Result<()> {
    for mount in config_mounts {
        if mounts::get_mount(conn, &mount.path)?.is_some() {
            continue;
        }
        mounts::upsert_mount(
            conn,
            &mount.path,
            &mount.folder_id,
            mount.grant_id.as_deref(),
            mount.scope_node_id.as_deref(),
            mount.mount_token.as_deref(),
            true,
        )?;
    }
    Ok(())
}

pub(crate) fn load_config(path: &Path) -> Result<DaemonConfig> {
    let text = fs::read_to_string(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            anyhow!("Config not found. Run: valv daemon install")
        } else {
            err.into()
        }
    })?;
    parse_config(&text)
}

pub(crate) fn ensure_config_template() -> Result<()> {
    let path = config_path()?;
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let backend_url = prompt_value("Backend URL", "https://api.valvsync.com")?;
    let device_name = prompt_value("Device name", &default_device_name())?;
    let contents = format!(
        r#"backend_url = "{}"
device_id = ""
device_token = ""
device_name = "{}"
mounts = []
"#,
        toml_escape(&backend_url),
        toml_escape(&device_name)
    );
    fs::write(&path, contents)?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

fn prompt_value(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let value = input.trim();
    if value.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(value.to_owned())
    }
}

fn parse_config(text: &str) -> Result<DaemonConfig> {
    let raw: RawDaemonConfig = toml::from_str(text)?;
    let backend_url = required_config_value(raw.backend_url, "backend_url")?;
    let device_id = required_config_value(raw.device_id, "device_id")?;
    let device_token = required_config_value(raw.device_token, "device_token")?;

    Ok(DaemonConfig {
        backend_url,
        device_id,
        device_token,
        device_name: raw.device_name.unwrap_or_else(default_device_name),
        mounts: raw.mounts,
    })
}

fn required_config_value(value: Option<String>, key: &str) -> Result<String> {
    let Some(value) = value else {
        return Err(anyhow!("Missing {key} in config.toml"));
    };
    if value.trim().is_empty() {
        return Err(anyhow!("Missing {key} in config.toml"));
    }
    Ok(value)
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub(crate) fn config_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".config/valv"))
}

pub(crate) fn data_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".local/share/valv"))
}

#[cfg(target_os = "macos")]
pub(crate) fn log_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Logs/Valv"))
}

#[cfg(target_os = "macos")]
pub(crate) const LOG_RETENTION_DAYS: u64 = 7;

#[cfg(target_os = "macos")]
pub(crate) fn prune_old_logs(log_dir: &Path) -> Result<()> {
    use std::time::{Duration, SystemTime};

    if !log_dir.exists() {
        return Ok(());
    }
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(LOG_RETENTION_DAYS * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("valvd") || !entry.file_type()?.is_file() {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if modified < cutoff {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub(crate) fn socket_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("valvd.sock"))
}

// Shared with the sandboxed macOS Xcode targets (Valv/ValvFileProvider/ValvFileProviderUI),
// which cannot reach `socket_path()` from inside App Sandbox (validated: EPERM on connect()
// regardless of entitlement) and instead read the TCP loopback port valvd advertises here.
pub(crate) fn tcp_port_file_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/Group Containers/group.dev.drnkn.valv")
        .join("valvd-tcp-port"))
}

pub(crate) fn resolve_valvd_path() -> Result<PathBuf> {
    let current = env::current_exe()?;
    if current
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "valvd")
    {
        return Ok(current);
    }
    if let Some(parent) = current.parent() {
        let sibling = parent.join("valvd");
        if sibling.exists() {
            return Ok(sibling);
        }
    }
    Ok(PathBuf::from("/usr/local/bin/valvd"))
}

pub(crate) fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))
}

pub(crate) fn default_device_name() -> String {
    env::var("HOSTNAME").unwrap_or_else(|_| "Valv Device".into())
}

pub(crate) fn set_owner_only_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use valv_sync::persistence::mounts;

    #[test]
    fn config_missing_backend_url_returns_actionable_error() {
        let err = parse_config(
            r#"
device_id = "device"
device_token = "token"
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("Missing backend_url"));
    }

    #[test]
    fn config_missing_device_token_returns_actionable_error() {
        let err = parse_config(
            r#"
backend_url = "https://api.valv.dev"
device_id = "device"
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("Missing device_token"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn log_dir_uses_macos_user_logs_location() {
        let path = log_dir().unwrap();

        assert_eq!(path, home_dir().unwrap().join("Library/Logs/Valv"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prune_old_logs_deletes_only_expired_valvd_files() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("valvd.log.2026-01-01");
        let new = dir.path().join("valvd.log");
        let unrelated = dir.path().join("other.log");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        fs::write(&unrelated, b"old").unwrap();
        let status = Command::new("touch")
            .arg("-t")
            .arg("200001010000")
            .arg(&old)
            .arg(&unrelated)
            .status()
            .unwrap();
        assert!(status.success());

        prune_old_logs(dir.path()).unwrap();

        assert!(!old.exists());
        assert!(new.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn config_defaults_device_name() {
        let config = parse_config(
            r#"
backend_url = "https://api.valv.dev"
device_id = "device"
device_token = "token"
"#,
        )
        .unwrap();

        assert!(!config.device_name.is_empty());
        assert!(config.mounts.is_empty());
    }

    #[test]
    fn config_empty_mounts_array_parses_as_empty_vec() {
        let config = parse_config(
            r#"
backend_url = "https://api.valv.dev"
device_id = "device"
device_token = "token"
mounts = []
"#,
        )
        .unwrap();

        assert!(config.mounts.is_empty());
    }

    #[test]
    fn merge_config_mounts_inserts_missing_config_mount() {
        let conn = test_conn();
        merge_config_mounts(
            &conn,
            &[MountConfig {
                path: "/sync".to_owned(),
                folder_id: "folder-1".to_owned(),
                grant_id: Some("grant-1".to_owned()),
                scope_node_id: Some("scope-1".to_owned()),
                mount_token: Some("mount-token-1".to_owned()),
            }],
        )
        .unwrap();

        let mount = mounts::get_mount(&conn, "/sync").unwrap().unwrap();
        assert_eq!(mount.folder_id, "folder-1");
        assert_eq!(mount.grant_id.as_deref(), Some("grant-1"));
        assert_eq!(mount.scope_node_id.as_deref(), Some("scope-1"));
        assert_eq!(mount.mount_token.as_deref(), Some("mount-token-1"));
        assert!(mount.can_write);
    }

    #[test]
    fn merge_config_mounts_does_not_overwrite_existing_mount_fields() {
        let conn = test_conn();
        mounts::upsert_mount(
            &conn,
            "/sync",
            "folder-existing",
            Some("grant-existing"),
            Some("scope-existing"),
            Some("token-existing"),
            true,
        )
        .unwrap();

        merge_config_mounts(
            &conn,
            &[MountConfig {
                path: "/sync".to_owned(),
                folder_id: "folder-stale".to_owned(),
                grant_id: Some("grant-stale".to_owned()),
                scope_node_id: Some("scope-stale".to_owned()),
                mount_token: Some("token-stale".to_owned()),
            }],
        )
        .unwrap();

        let mount = mounts::get_mount(&conn, "/sync").unwrap().unwrap();
        assert_eq!(mount.folder_id, "folder-existing");
        assert_eq!(mount.grant_id.as_deref(), Some("grant-existing"));
        assert_eq!(mount.scope_node_id.as_deref(), Some("scope-existing"));
        assert_eq!(mount.mount_token.as_deref(), Some("token-existing"));
        assert!(mount.can_write);
    }

    #[test]
    fn merge_config_mounts_does_not_flip_read_only_mount_writable() {
        let conn = test_conn();
        mounts::upsert_mount(&conn, "/sync", "folder-existing", None, None, None, false).unwrap();

        merge_config_mounts(
            &conn,
            &[MountConfig {
                path: "/sync".to_owned(),
                folder_id: "folder-stale".to_owned(),
                grant_id: None,
                scope_node_id: None,
                mount_token: None,
            }],
        )
        .unwrap();

        let mount = mounts::get_mount(&conn, "/sync").unwrap().unwrap();
        assert_eq!(mount.folder_id, "folder-existing");
        assert!(!mount.can_write);
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../valv-sync/src/persistence/schema.sql"))
            .unwrap();
        conn
    }
}
