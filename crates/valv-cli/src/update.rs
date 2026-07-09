
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use valv_sync::{
    protocol::ipc::DaemonStatus,
    update::{self as shared_update, is_newer_version, resolve_latest_version, verify_sha256sums},
};

use crate::daemon::daemon_client;

#[cfg(target_os = "macos")]
use crate::paths::{app_managed_bin_dir, launch_agent_plist_path};

const APP_MANAGED_NOTICE: &str = "valvd is managed by the Valv app — update the app instead";
const RESTART_POLL_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) async fn cmd_update(check: bool) -> Result<()> {
    let guard = check_managed_install_guard()?;
    if guard == ManagedInstallGuard::FullAbort {
        println!("{APP_MANAGED_NOTICE}");
        return Ok(());
    }
    let daemon_is_app_managed = guard == ManagedInstallGuard::DaemonManaged;

    let current_version = env!("CARGO_PKG_VERSION");
    let client = reqwest::Client::new();
    let latest_version =
        resolve_latest_version(&client, shared_update::DEFAULT_REPO, "VALV_VERSION")
            .await
            .context("failed to resolve the latest valv release")?;

    match plan_update(current_version, &latest_version, check) {
        UpdatePlan::AlreadyUpToDate => {
            println!("valv is already up to date ({current_version})");
            return Ok(());
        }
        UpdatePlan::ReportOnly => {
            println!(
                "A newer version of valv is available ({latest_version}). Run 'valv update' to install it."
            );
            return Ok(());
        }
        UpdatePlan::Install => {}
    }

    let current_exe = env::current_exe().context("failed to determine current executable path")?;
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?
        .to_path_buf();

    let target = detect_target(env::consts::OS, env::consts::ARCH)?;
    let asset = format!("valv-{latest_version}-{target}.tar.gz");
    let release_base = format!(
        "https://github.com/{}/releases/download/v{latest_version}",
        shared_update::DEFAULT_REPO
    );

    let tarball_bytes = download_bytes(&client, &format!("{release_base}/{asset}")).await?;
    let sha256sums_bytes =
        download_bytes(&client, &format!("{release_base}/SHA256SUMS")).await?;
    let minisig_bytes =
        download_bytes(&client, &format!("{release_base}/SHA256SUMS.minisig")).await?;

    verify_tarball_checksum(&asset, &tarball_bytes, &sha256sums_bytes)?;
    verify_sha256sums(&sha256sums_bytes, &minisig_bytes)
        .context("SHA256SUMS.minisig did not verify against SHA256SUMS")?;

    let extract_dir = extract_tarball(&tarball_bytes)?;
    let staged_valv = extract_dir.join("valv");
    let staged_valvd = extract_dir.join("valvd");
    if !staged_valv.exists() || !staged_valvd.exists() {
        return Err(anyhow!("{asset} did not contain valv and valvd"));
    }

    let valvd_sibling_present = install_dir.join("valvd").exists();
    let should_swap_valvd = valvd_sibling_present && !daemon_is_app_managed;

    let swap = platform_binary_swap();
    let backup = swap.swap(
        &install_dir,
        &staged_valv,
        should_swap_valvd.then_some(staged_valvd.as_path()),
    )?;
    let _ = fs::remove_dir_all(&extract_dir);

    if should_swap_valvd {
        if let Err(error) = restart_and_confirm(&latest_version).await {
            swap.rollback(&backup)?;
            let _ = restart_daemon();
            return Err(anyhow!(
                "update failed and was rolled back: {error}"
            ));
        }
    }

    if daemon_is_app_managed {
        println!("Updated valv: {current_version} -> {latest_version}");
        println!("{APP_MANAGED_NOTICE}");
    } else if valvd_sibling_present {
        println!("Updated valv and valvd: {current_version} -> {latest_version}");
    } else {
        println!("Updated valv: {current_version} -> {latest_version} (no valvd sibling found; daemon not restarted)");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdatePlan {
    AlreadyUpToDate,
    ReportOnly,
    Install,
}

fn plan_update(current_version: &str, latest_version: &str, check: bool) -> UpdatePlan {
    if !is_newer_version(latest_version, current_version) {
        return UpdatePlan::AlreadyUpToDate;
    }
    if check {
        return UpdatePlan::ReportOnly;
    }
    UpdatePlan::Install
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedInstallGuard {
    None,
    FullAbort,
    DaemonManaged,
}

fn classify_managed_install(
    app_bin_dir: &Path,
    current_exe_parent: Option<&Path>,
    registered_valvd_path: Option<&str>,
) -> ManagedInstallGuard {
    if current_exe_parent == Some(app_bin_dir) {
        return ManagedInstallGuard::FullAbort;
    }
    if let Some(registered) = registered_valvd_path {
        if Path::new(registered).starts_with(app_bin_dir) {
            return ManagedInstallGuard::DaemonManaged;
        }
    }
    ManagedInstallGuard::None
}

#[cfg(target_os = "macos")]
fn check_managed_install_guard() -> Result<ManagedInstallGuard> {
    let app_bin_dir = app_managed_bin_dir().context("failed to resolve app-managed bin dir")?;
    let current_exe = env::current_exe().context("failed to determine current executable path")?;

    let plist_path = launch_agent_plist_path().context("failed to resolve LaunchAgent plist path")?;
    let registered_valvd_path = read_registered_valvd_path(&plist_path);

    Ok(classify_managed_install(
        &app_bin_dir,
        current_exe.parent(),
        registered_valvd_path.as_deref(),
    ))
}

#[cfg(not(target_os = "macos"))]
fn check_managed_install_guard() -> Result<ManagedInstallGuard> {
    Ok(ManagedInstallGuard::None)
}

#[cfg(target_os = "macos")]
fn read_registered_valvd_path(plist_path: &Path) -> Option<String> {
    let contents = fs::read_to_string(plist_path).ok()?;
    parse_first_program_argument(&contents)
}

fn parse_first_program_argument(plist_contents: &str) -> Option<String> {
    const KEY_MARKER: &str = "<key>ProgramArguments</key>";
    let key_index = plist_contents.find(KEY_MARKER)?;
    let after_key = &plist_contents[key_index + KEY_MARKER.len()..];
    let array_start = after_key.find("<array>")? + "<array>".len();
    let array_end = after_key.find("</array>")?;
    if array_end < array_start {
        return None;
    }
    let array_contents = &after_key[array_start..array_end];
    let string_start = array_contents.find("<string>")? + "<string>".len();
    let string_end = array_contents.find("</string>")?;
    if string_end < string_start {
        return None;
    }
    Some(xml_unescape(&array_contents[string_start..string_end]))
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}


fn detect_target(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => Err(anyhow!(
            "unsupported platform {os}/{arch}; supported targets are macOS arm64 and Linux x86_64"
        )),
    }
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to download {url}"))?;
    if !response.status().is_success() {
        return Err(anyhow!("failed to download {url}: HTTP {}", response.status()));
    }
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?
        .to_vec())
}

fn verify_tarball_checksum(asset: &str, tarball_bytes: &[u8], sha256sums_bytes: &[u8]) -> Result<()> {
    let sha256sums = std::str::from_utf8(sha256sums_bytes).context("SHA256SUMS is not valid UTF-8")?;
    let expected = checksum_for_asset(sha256sums, asset)
        .ok_or_else(|| anyhow!("SHA256SUMS does not contain {asset}"))?;
    let actual = sha256_hex(tarball_bytes);
    if actual != expected {
        return Err(anyhow!(
            "checksum mismatch for {asset}: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn checksum_for_asset(sha256sums: &str, asset: &str) -> Option<String> {
    for line in sha256sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name == asset || name == format!("*{asset}") {
            return Some(hash.to_owned());
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn extract_tarball(tarball_bytes: &[u8]) -> Result<PathBuf> {
    let extract_dir = env::temp_dir().join(format!("valv-update-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&extract_dir)
        .with_context(|| format!("failed to create {}", extract_dir.display()))?;
    let tarball_path = extract_dir.join("download.tar.gz");
    fs::write(&tarball_path, tarball_bytes)
        .with_context(|| format!("failed to write {}", tarball_path.display()))?;

    let status = ProcessCommand::new("tar")
        .arg("-xzf")
        .arg(&tarball_path)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        return Err(anyhow!("tar extraction failed with status {status}"));
    }
    let _ = fs::remove_file(&tarball_path);
    Ok(extract_dir)
}


#[derive(Debug, Clone)]
pub(crate) struct SwapBackup {
    valv_current: PathBuf,
    valv_backup: PathBuf,
    valvd_current: Option<PathBuf>,
    valvd_backup: Option<PathBuf>,
}

pub(crate) trait BinarySwap {
    fn swap(
        &self,
        install_dir: &Path,
        staged_valv: &Path,
        staged_valvd: Option<&Path>,
    ) -> Result<SwapBackup>;
    fn rollback(&self, backup: &SwapBackup) -> Result<()>;
}

#[cfg(unix)]
pub(crate) struct UnixBinarySwap;

#[cfg(unix)]
impl BinarySwap for UnixBinarySwap {
    fn swap(
        &self,
        install_dir: &Path,
        staged_valv: &Path,
        staged_valvd: Option<&Path>,
    ) -> Result<SwapBackup> {
        let valv_current = install_dir.join("valv");
        let valv_backup = install_dir.join("valv.old");
        rename_atomic(&valv_current, &valv_backup)?;
        if let Err(error) = rename_atomic(staged_valv, &valv_current) {
            let _ = rename_atomic(&valv_backup, &valv_current);
            return Err(error.context("failed to install new valv binary"));
        }
        set_executable(&valv_current)?;

        let (valvd_current, valvd_backup) = match staged_valvd {
            Some(staged_valvd) => {
                let valvd_current = install_dir.join("valvd");
                let valvd_backup = install_dir.join("valvd.old");
                rename_atomic(&valvd_current, &valvd_backup)?;
                if let Err(error) = rename_atomic(staged_valvd, &valvd_current) {
                    let _ = rename_atomic(&valvd_backup, &valvd_current);
                    let _ = rename_atomic(&valv_backup, &valv_current);
                    return Err(error.context("failed to install new valvd binary"));
                }
                set_executable(&valvd_current)?;
                (Some(valvd_current), Some(valvd_backup))
            }
            None => (None, None),
        };

        Ok(SwapBackup {
            valv_current,
            valv_backup,
            valvd_current,
            valvd_backup,
        })
    }

    fn rollback(&self, backup: &SwapBackup) -> Result<()> {
        rename_atomic(&backup.valv_backup, &backup.valv_current)
            .context("failed to roll back valv binary")?;
        if let (Some(valvd_current), Some(valvd_backup)) = (&backup.valvd_current, &backup.valvd_backup) {
            rename_atomic(valvd_backup, valvd_current).context("failed to roll back valvd binary")?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn rename_atomic(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)
        .with_context(|| format!("failed to rename {} -> {}", from.display(), to.display()))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("failed to set executable permission on {}", path.display()))
}

#[cfg(windows)]
pub(crate) struct WindowsBinarySwap;

#[cfg(windows)]
impl BinarySwap for WindowsBinarySwap {
    fn swap(
        &self,
        _install_dir: &Path,
        _staged_valv: &Path,
        _staged_valvd: Option<&Path>,
    ) -> Result<SwapBackup> {
        unimplemented!(
            "valv update is not yet supported on Windows - see cli-daemon-self-update design.md D4"
        )
    }

    fn rollback(&self, _backup: &SwapBackup) -> Result<()> {
        unimplemented!(
            "valv update is not yet supported on Windows - see cli-daemon-self-update design.md D4"
        )
    }
}

#[cfg(unix)]
fn platform_binary_swap() -> UnixBinarySwap {
    UnixBinarySwap
}

#[cfg(windows)]
fn platform_binary_swap() -> WindowsBinarySwap {
    WindowsBinarySwap
}


#[cfg(target_os = "macos")]
fn restart_daemon() -> Result<()> {
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
fn restart_daemon() -> Result<()> {
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
fn restart_daemon() -> Result<()> {
    Err(anyhow!("daemon restart is not supported on this platform"))
}

async fn restart_and_confirm(target_version: &str) -> Result<()> {
    restart_daemon()?;
    if poll_daemon_version(target_version, RESTART_POLL_TIMEOUT).await {
        Ok(())
    } else {
        Err(anyhow!(
            "daemon did not report version {target_version} within {}s",
            RESTART_POLL_TIMEOUT.as_secs()
        ))
    }
}

async fn poll_daemon_version(target_version: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if daemon_reports_version(target_version).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(RESTART_POLL_INTERVAL).await;
    }
}

async fn daemon_reports_version(target_version: &str) -> bool {
    let Ok(client) = daemon_client() else {
        return false;
    };
    let Ok(response) = client.get("http://localhost/status").send().await else {
        return false;
    };
    let Ok(status) = response.json::<DaemonStatus>().await else {
        return false;
    };
    daemon_status_matches_version(&status, target_version)
}

fn daemon_status_matches_version(status: &DaemonStatus, target_version: &str) -> bool {
    status.version == target_version
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_update_reports_already_up_to_date() {
        assert_eq!(
            plan_update("0.2.0", "0.2.0", false),
            UpdatePlan::AlreadyUpToDate
        );
        assert_eq!(
            plan_update("0.2.0", "0.1.0", false),
            UpdatePlan::AlreadyUpToDate
        );
    }

    #[test]
    fn plan_update_check_reports_without_installing() {
        assert_eq!(plan_update("0.1.0", "0.2.0", true), UpdatePlan::ReportOnly);
    }

    #[test]
    fn plan_update_installs_when_newer_and_not_check() {
        assert_eq!(plan_update("0.1.0", "0.2.0", false), UpdatePlan::Install);
    }

    #[test]
    fn detect_target_maps_supported_platforms() {
        assert_eq!(
            detect_target("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            detect_target("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn detect_target_rejects_unsupported_platforms() {
        assert!(detect_target("windows", "x86_64").is_err());
        assert!(detect_target("macos", "x86_64").is_err());
    }

    #[test]
    fn checksum_for_asset_matches_plain_and_binary_mode_lines() {
        let sha256sums = "abc123  valv-0.1.0-aarch64-apple-darwin.tar.gz\ndef456 *valv-0.1.0-x86_64-unknown-linux-gnu.tar.gz\n";
        assert_eq!(
            checksum_for_asset(sha256sums, "valv-0.1.0-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            checksum_for_asset(sha256sums, "valv-0.1.0-x86_64-unknown-linux-gnu.tar.gz").as_deref(),
            Some("def456")
        );
        assert!(checksum_for_asset(sha256sums, "missing.tar.gz").is_none());
    }

    #[test]
    fn verify_tarball_checksum_rejects_a_tampered_download() {
        let tarball_bytes = b"real tarball bytes";
        let expected = sha256_hex(tarball_bytes);
        let sha256sums = format!("{expected}  valv-0.1.0-aarch64-apple-darwin.tar.gz\n");

        assert!(verify_tarball_checksum(
            "valv-0.1.0-aarch64-apple-darwin.tar.gz",
            tarball_bytes,
            sha256sums.as_bytes()
        )
        .is_ok());

        let tampered = b"tampered tarball bytes";
        assert!(verify_tarball_checksum(
            "valv-0.1.0-aarch64-apple-darwin.tar.gz",
            tampered,
            sha256sums.as_bytes()
        )
        .is_err());
    }

    #[test]
    fn classify_managed_install_full_aborts_when_running_from_app_bin_dir() {
        let app_bin_dir = Path::new("/Users/tester/Library/Application Support/Valv/bin");
        let outcome = classify_managed_install(app_bin_dir, Some(app_bin_dir), None);
        assert_eq!(outcome, ManagedInstallGuard::FullAbort);
    }

    #[test]
    fn classify_managed_install_flags_mixed_install_when_daemon_is_app_managed() {
        let app_bin_dir = Path::new("/Users/tester/Library/Application Support/Valv/bin");
        let curl_installed = Path::new("/Users/tester/.local/bin");
        let registered = "/Users/tester/Library/Application Support/Valv/bin/valvd";

        let outcome = classify_managed_install(app_bin_dir, Some(curl_installed), Some(registered));

        assert_eq!(outcome, ManagedInstallGuard::DaemonManaged);
    }

    #[test]
    fn classify_managed_install_none_when_daemon_is_not_app_managed() {
        let app_bin_dir = Path::new("/Users/tester/Library/Application Support/Valv/bin");
        let curl_installed = Path::new("/Users/tester/.local/bin");
        let registered = "/Users/tester/.local/bin/valvd";

        let outcome = classify_managed_install(app_bin_dir, Some(curl_installed), Some(registered));

        assert_eq!(outcome, ManagedInstallGuard::None);
    }

    #[test]
    fn classify_managed_install_none_when_no_daemon_registered() {
        let app_bin_dir = Path::new("/Users/tester/Library/Application Support/Valv/bin");
        let curl_installed = Path::new("/Users/tester/.local/bin");

        let outcome = classify_managed_install(app_bin_dir, Some(curl_installed), None);

        assert_eq!(outcome, ManagedInstallGuard::None);
    }

    #[test]
    fn parse_first_program_argument_extracts_the_binary_path() {
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.drnkn.valvd</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/tester/Library/Application Support/Valv/bin/valvd</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#;

        let path = parse_first_program_argument(plist).unwrap();

        assert_eq!(
            path,
            "/Users/tester/Library/Application Support/Valv/bin/valvd"
        );
    }

    #[test]
    fn parse_first_program_argument_returns_none_without_the_key() {
        assert!(parse_first_program_argument("<plist></plist>").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn unix_binary_swap_and_rollback_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path();
        fs::write(install_dir.join("valv"), b"old valv").unwrap();
        fs::write(install_dir.join("valvd"), b"old valvd").unwrap();
        let staged_valv = install_dir.join("staged-valv");
        let staged_valvd = install_dir.join("staged-valvd");
        fs::write(&staged_valv, b"new valv").unwrap();
        fs::write(&staged_valvd, b"new valvd").unwrap();

        let swap = UnixBinarySwap;
        let backup = swap
            .swap(install_dir, &staged_valv, Some(staged_valvd.as_path()))
            .unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"new valv");
        assert_eq!(fs::read(install_dir.join("valvd")).unwrap(), b"new valvd");
        assert_eq!(
            fs::read(install_dir.join("valv.old")).unwrap(),
            b"old valv"
        );

        swap.rollback(&backup).unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"old valv");
        assert_eq!(fs::read(install_dir.join("valvd")).unwrap(), b"old valvd");
    }

    #[cfg(unix)]
    #[test]
    fn unix_binary_swap_skips_valvd_when_no_sibling_present() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path();
        fs::write(install_dir.join("valv"), b"old valv").unwrap();
        let staged_valv = install_dir.join("staged-valv");
        fs::write(&staged_valv, b"new valv").unwrap();

        let swap = UnixBinarySwap;
        let backup = swap.swap(install_dir, &staged_valv, None).unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"new valv");
        assert!(!install_dir.join("valvd").exists());
        assert!(backup.valvd_current.is_none());
        assert!(backup.valvd_backup.is_none());
    }

    #[test]
    fn daemon_status_matches_version_compares_exact_string() {
        let status = DaemonStatus {
            paused: false,
            backend_connected: true,
            version: "0.2.0".into(),
            update_required: false,
            mounts: vec![],
            account: None,
            latest_version: None,
            update_available: None,
        };
        assert!(daemon_status_matches_version(&status, "0.2.0"));
        assert!(!daemon_status_matches_version(&status, "0.1.0"));
    }
}
