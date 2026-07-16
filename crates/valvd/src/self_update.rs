use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{anyhow, Context, Result};
use valv_sync::update::{
    component_asset_name, component_release_base, detect_target, download_release_asset,
    extract_tarball, verify_sha256sums, verify_tarball_checksum, Component,
};

pub(crate) fn reap_stale_backup() {
    let Ok(current_exe) = env::current_exe() else {
        return;
    };
    remove_stale_backup(&current_exe);
}

fn remove_stale_backup(current_exe: &Path) {
    let _ = fs::remove_file(current_exe.with_extension("old"));
}

pub(crate) fn is_app_managed_install() -> bool {
    let Some(app_bin_dir) = app_managed_bin_dir() else {
        return false;
    };
    let Ok(current_exe) = env::current_exe() else {
        return false;
    };
    current_exe.parent() == Some(app_bin_dir.as_path())
}

#[cfg(target_os = "macos")]
fn app_managed_bin_dir() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Valv/bin"))
}

#[cfg(not(target_os = "macos"))]
fn app_managed_bin_dir() -> Option<PathBuf> {
    None
}

pub(crate) async fn attempt_self_update(
    client: &reqwest::Client,
    repo: &str,
    latest_version: &str,
) -> Result<()> {
    let target = detect_target(env::consts::OS, env::consts::ARCH)?;
    let asset = component_asset_name("valvd", latest_version, target);
    let release_base = component_release_base(repo, Component::Valvd, latest_version);

    let tarball_bytes = download_release_asset(client, &format!("{release_base}/{asset}")).await?;
    let sha256sums_bytes =
        download_release_asset(client, &format!("{release_base}/SHA256SUMS")).await?;
    let minisig_bytes =
        download_release_asset(client, &format!("{release_base}/SHA256SUMS.minisig")).await?;

    verify_tarball_checksum(&asset, &tarball_bytes, &sha256sums_bytes)?;
    verify_sha256sums(&sha256sums_bytes, &minisig_bytes)
        .context("SHA256SUMS.minisig did not verify against SHA256SUMS")?;

    let extract_dir = extract_tarball(&tarball_bytes)?;
    let staged_path = extract_dir.join("valvd");
    if !staged_path.exists() {
        let _ = fs::remove_dir_all(&extract_dir);
        return Err(anyhow!("{asset} did not contain valvd"));
    }

    let current_exe = env::current_exe().context("failed to determine current executable path")?;
    swap_and_restart(&current_exe, &staged_path, &extract_dir, trigger_restart)
}

// The extract dir is removed once the swap has landed but before `restart` runs,
// because on macOS `restart` SIGKILLs this process (`launchctl kickstart -k`) and
// nothing after that call ever executes.
fn swap_and_restart(
    current_exe: &Path,
    staged_path: &Path,
    extract_dir: &Path,
    restart: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let backup_path = current_exe.with_extension("old");
    rename_atomic(current_exe, &backup_path)?;

    if let Err(error) = rename_atomic(staged_path, current_exe) {
        restore_binary(&backup_path, current_exe);
        let _ = fs::remove_dir_all(extract_dir);
        return Err(error.context("failed to install new valvd binary"));
    }
    if let Err(error) = set_executable(current_exe) {
        restore_binary(&backup_path, current_exe);
        let _ = fs::remove_dir_all(extract_dir);
        return Err(error.context("failed to set executable permission on new valvd binary"));
    }

    let _ = fs::remove_dir_all(extract_dir);

    if let Err(error) = restart() {
        restore_binary(&backup_path, current_exe);
        return Err(error.context("failed to trigger valvd restart after self-update"));
    }

    let _ = fs::remove_file(&backup_path);
    Ok(())
}

fn rename_atomic(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)
        .with_context(|| format!("failed to rename {} -> {}", from.display(), to.display()))
}

fn restore_binary(backup: &Path, current: &Path) {
    let _ = rename_atomic(backup, current);
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("failed to set executable permission on {}", path.display()))
}

#[cfg(target_os = "macos")]
fn trigger_restart() -> Result<()> {
    let uid_output = ProcessCommand::new("id")
        .arg("-u")
        .output()
        .context("failed to run id -u")?;
    if !uid_output.status.success() {
        return Err(anyhow!("id -u failed with status {}", uid_output.status));
    }
    let uid = String::from_utf8(uid_output.stdout)
        .context("id -u output was not valid UTF-8")?
        .trim()
        .to_owned();
    let status = ProcessCommand::new("launchctl")
        .args(["kickstart", "-k", &format!("gui/{uid}/dev.drnkn.valvd")])
        .status()
        .context("failed to run launchctl kickstart")?;
    if !status.success() {
        return Err(anyhow!("launchctl kickstart failed with status {status}"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn trigger_restart() -> Result<()> {
    let status = ProcessCommand::new("systemctl")
        .args(["--user", "restart", "valvd"])
        .status()
        .context("failed to run systemctl --user restart valvd")?;
    if !status.success() {
        return Err(anyhow!(
            "systemctl --user restart valvd failed with status {status}"
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn trigger_restart() -> Result<()> {
    Err(anyhow!(
        "valvd self-update restart is not supported on this platform"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_app_managed_install_is_false_outside_the_app_bin_dir() {
        assert!(!is_app_managed_install());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn app_managed_bin_dir_is_none_off_macos() {
        assert!(app_managed_bin_dir().is_none());
    }

    fn make_extract_dir(dir: &Path) -> PathBuf {
        let extract_dir = dir.join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        fs::write(extract_dir.join("valvd"), b"new valvd").unwrap();
        extract_dir
    }

    #[test]
    fn swap_and_restart_installs_the_staged_binary_when_restart_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = dir.path().join("valvd");
        let extract_dir = make_extract_dir(dir.path());
        let staged = extract_dir.join("valvd");
        fs::write(&current_exe, b"old valvd").unwrap();

        swap_and_restart(&current_exe, &staged, &extract_dir, || Ok(())).unwrap();

        assert_eq!(fs::read(&current_exe).unwrap(), b"new valvd");
        assert!(!dir.path().join("valvd.old").exists());
        assert!(!extract_dir.exists());
    }

    #[test]
    fn swap_and_restart_removes_the_extract_dir_before_invoking_restart() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = dir.path().join("valvd");
        let extract_dir = make_extract_dir(dir.path());
        let staged = extract_dir.join("valvd");
        fs::write(&current_exe, b"old valvd").unwrap();

        swap_and_restart(&current_exe, &staged, &extract_dir, || {
            assert!(
                !extract_dir.exists(),
                "extract dir must be gone before the process-killing restart runs"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn swap_and_restart_rolls_back_when_the_restart_trigger_fails() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = dir.path().join("valvd");
        let extract_dir = make_extract_dir(dir.path());
        let staged = extract_dir.join("valvd");
        fs::write(&current_exe, b"old valvd").unwrap();

        let result = swap_and_restart(&current_exe, &staged, &extract_dir, || {
            Err(anyhow!("launchctl kickstart failed"))
        });

        assert!(result.is_err());
        assert_eq!(fs::read(&current_exe).unwrap(), b"old valvd");
        assert!(!dir.path().join("valvd.old").exists());
    }

    #[test]
    fn swap_and_restart_rolls_back_when_the_staged_binary_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = dir.path().join("valvd");
        let extract_dir = dir.path().join("extract");
        fs::create_dir_all(&extract_dir).unwrap();
        let missing_staged = extract_dir.join("valvd-missing");
        fs::write(&current_exe, b"old valvd").unwrap();

        let result = swap_and_restart(&current_exe, &missing_staged, &extract_dir, || Ok(()));

        assert!(result.is_err());
        assert_eq!(fs::read(&current_exe).unwrap(), b"old valvd");
        assert!(!dir.path().join("valvd.old").exists());
        assert!(!extract_dir.exists());
    }

    #[test]
    fn remove_stale_backup_removes_the_backup_next_to_the_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = dir.path().join("valvd");
        let backup_path = current_exe.with_extension("old");
        fs::write(&backup_path, b"stale backup").unwrap();

        remove_stale_backup(&current_exe);

        assert!(!backup_path.exists());
    }

    #[test]
    fn remove_stale_backup_is_a_no_op_when_there_is_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = dir.path().join("valvd");

        remove_stale_backup(&current_exe);

        assert!(!current_exe.with_extension("old").exists());
    }
}
