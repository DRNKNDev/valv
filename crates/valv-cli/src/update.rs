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
    update::{self as shared_update, is_newer_version, resolve_latest_version, verify_sha256sums, Component},
};

use crate::daemon::daemon_client;

#[cfg(target_os = "macos")]
use crate::paths::{app_managed_bin_dir, launch_agent_plist_path};

const APP_MANAGED_NOTICE: &str = "valvd is managed by the Valv app. Update the app instead.";
const RESTART_POLL_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_POLL_INTERVAL: Duration = Duration::from_millis(250);

fn emit(json: bool, human_lines: &[String], value: serde_json::Value) {
    if json {
        println!("{value}");
    } else {
        for line in human_lines {
            println!("{line}");
        }
    }
}

struct StagedComponent {
    extract_dir: PathBuf,
    staged_path: PathBuf,
    old_version: String,
    new_version: String,
}

pub(crate) async fn cmd_update(json: bool) -> Result<()> {
    let guard = check_managed_install_guard()?;
    if guard == ManagedInstallGuard::FullAbort {
        emit(
            json,
            &[APP_MANAGED_NOTICE.to_owned()],
            serde_json::json!({"result": "app_managed"}),
        );
        return Ok(());
    }
    let daemon_is_app_managed = guard == ManagedInstallGuard::DaemonManaged;

    let current_exe = env::current_exe().context("failed to determine current executable path")?;
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?
        .to_path_buf();
    let valvd_sibling_present = install_dir.join("valvd").exists();
    let should_consider_valvd = valvd_sibling_present && !daemon_is_app_managed;

    let client = reqwest::Client::new();
    let repo = shared_update::DEFAULT_REPO;

    let current_cli_version = env!("CARGO_PKG_VERSION").to_owned();
    let cli_pinned = env_pin_is_set("VALV_CLI_VERSION");
    let latest_cli_version = resolve_latest_version(&client, repo, Component::Cli, "VALV_CLI_VERSION")
        .await
        .context("failed to resolve the latest valv release")?;
    let cli_action = plan_update(&current_cli_version, &latest_cli_version, cli_pinned);

    let valvd_info = if should_consider_valvd {
        let valvd_pinned = env_pin_is_set("VALVD_VERSION");
        let latest_valvd_version = resolve_latest_version(&client, repo, Component::Valvd, "VALVD_VERSION")
            .await
            .context("failed to resolve the latest valvd release")?;
        let current_valvd_version = match current_daemon_version().await {
            Some(version) => version,
            None => installed_binary_version(&install_dir.join("valvd"))
                .unwrap_or_else(|| "0.0.0".to_owned()),
        };
        let action = plan_update(&current_valvd_version, &latest_valvd_version, valvd_pinned);
        Some((action, current_valvd_version, latest_valvd_version))
    } else {
        None
    };
    let valvd_action = valvd_info
        .as_ref()
        .map(|(action, _, _)| *action)
        .unwrap_or(UpdatePlan::AlreadyUpToDate);

    if cli_action == UpdatePlan::AlreadyUpToDate && valvd_action == UpdatePlan::AlreadyUpToDate {
        emit(
            json,
            &[format!("valv is already up to date ({current_cli_version})")],
            serde_json::json!({"result": "up_to_date", "version": current_cli_version}),
        );
        return Ok(());
    }

    let target = detect_target(env::consts::OS, env::consts::ARCH)?;

    let cli_staged = if cli_action == UpdatePlan::Install {
        Some(
            stage_component(
                &client,
                repo,
                Component::Cli,
                "valv",
                &latest_cli_version,
                target,
                current_cli_version.clone(),
            )
            .await?,
        )
    } else {
        None
    };
    let valvd_staged = if let Some((UpdatePlan::Install, current_valvd_version, latest_valvd_version)) =
        &valvd_info
    {
        Some(
            stage_component(
                &client,
                repo,
                Component::Valvd,
                "valvd",
                latest_valvd_version,
                target,
                current_valvd_version.clone(),
            )
            .await?,
        )
    } else {
        None
    };

    let swap = platform_binary_swap();
    let backup = swap.swap(
        &install_dir,
        cli_staged.as_ref().map(|component| component.staged_path.as_path()),
        valvd_staged.as_ref().map(|component| component.staged_path.as_path()),
    )?;

    if let Some(component) = &cli_staged {
        let _ = fs::remove_dir_all(&component.extract_dir);
    }
    if let Some(component) = &valvd_staged {
        let _ = fs::remove_dir_all(&component.extract_dir);
    }

    if let Some(valvd_component) = &valvd_staged {
        if let Err(error) = restart_and_confirm(&valvd_component.new_version).await {
            return Err(handle_failed_restart(&swap, &backup, restart_daemon, error));
        }
    }

    let mut updated_lines = Vec::new();
    let mut components_json = Vec::new();
    if let Some(component) = &cli_staged {
        updated_lines.push(format!("valv {} -> {}", component.old_version, component.new_version));
        components_json.push(serde_json::json!({
            "name": "valv",
            "from": component.old_version,
            "to": component.new_version,
        }));
    }
    if let Some(component) = &valvd_staged {
        updated_lines.push(format!("valvd {} -> {}", component.old_version, component.new_version));
        components_json.push(serde_json::json!({
            "name": "valvd",
            "from": component.old_version,
            "to": component.new_version,
        }));
    }

    let no_valvd_sibling = !valvd_sibling_present && !daemon_is_app_managed;
    let mut message = format!("Updated {}", updated_lines.join(", "));
    if no_valvd_sibling && valvd_staged.is_none() && cli_staged.is_some() {
        message.push_str(" (no valvd sibling found; daemon not restarted)");
    }

    let mut human_lines = vec![message];
    if daemon_is_app_managed && cli_staged.is_some() {
        human_lines.push(APP_MANAGED_NOTICE.to_owned());
    }

    emit(
        json,
        &human_lines,
        serde_json::json!({
            "result": "updated",
            "components": components_json,
            "valvd_restarted": valvd_staged.is_some(),
            "daemon_app_managed": daemon_is_app_managed,
        }),
    );
    Ok(())
}

fn env_pin_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty())
}

async fn stage_component(
    client: &reqwest::Client,
    repo: &str,
    component: Component,
    binary_name: &'static str,
    version: &str,
    target: &str,
    old_version: String,
) -> Result<StagedComponent> {
    let asset = component_asset_name(binary_name, version, target);
    let release_base = component_release_base(repo, component, version);

    let tarball_bytes = download_bytes(client, &format!("{release_base}/{asset}")).await?;
    let sha256sums_bytes = download_bytes(client, &format!("{release_base}/SHA256SUMS")).await?;
    let minisig_bytes = download_bytes(client, &format!("{release_base}/SHA256SUMS.minisig")).await?;

    verify_tarball_checksum(&asset, &tarball_bytes, &sha256sums_bytes)?;
    verify_sha256sums(&sha256sums_bytes, &minisig_bytes)
        .context("SHA256SUMS.minisig did not verify against SHA256SUMS")?;

    let extract_dir = extract_tarball(&tarball_bytes)?;
    let staged_path = extract_dir.join(binary_name);
    if !staged_path.exists() {
        return Err(anyhow!("{asset} did not contain {binary_name}"));
    }

    Ok(StagedComponent {
        extract_dir,
        staged_path,
        old_version,
        new_version: version.to_owned(),
    })
}

fn component_asset_name(binary_name: &str, version: &str, target: &str) -> String {
    format!("{binary_name}-{version}-{target}.tar.gz")
}

fn component_release_base(repo: &str, component: Component, version: &str) -> String {
    format!(
        "https://github.com/{repo}/releases/download/{}{version}",
        component.tag_prefix()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdatePlan {
    AlreadyUpToDate,
    Install,
}

fn plan_update(current_version: &str, latest_version: &str, pinned: bool) -> UpdatePlan {
    if !pinned && !is_newer_version(latest_version, current_version) {
        return UpdatePlan::AlreadyUpToDate;
    }
    UpdatePlan::Install
}

fn handle_failed_restart(
    swap: &impl BinarySwap,
    backup: &SwapBackup,
    restart: impl Fn() -> Result<()>,
    original_error: anyhow::Error,
) -> anyhow::Error {
    match swap.rollback(backup) {
        Ok(()) => {
            let _ = restart();
            anyhow!("update failed and was rolled back: {original_error}")
        }
        Err(rollback_error) => {
            let _ = restart();
            anyhow!(
                "update failed ({original_error}) AND the rollback also failed ({rollback_error}) - \
                 the installed valv/valvd binaries may be in an inconsistent state; \
                 reinstall with install.sh"
            )
        }
    }
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

    let plist_path =
        launch_agent_plist_path().context("failed to resolve LaunchAgent plist path")?;
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
        return Err(anyhow!(
            "failed to download {url}: HTTP {}",
            response.status()
        ));
    }
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?
        .to_vec())
}

fn verify_tarball_checksum(
    asset: &str,
    tarball_bytes: &[u8],
    sha256sums_bytes: &[u8],
) -> Result<()> {
    let sha256sums =
        std::str::from_utf8(sha256sums_bytes).context("SHA256SUMS is not valid UTF-8")?;
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
    let extract_dir =
        env::temp_dir().join(format!("valv-update-{}", uuid::Uuid::new_v4().simple()));
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
    valv_current: Option<PathBuf>,
    valv_backup: Option<PathBuf>,
    valvd_current: Option<PathBuf>,
    valvd_backup: Option<PathBuf>,
}

pub(crate) trait BinarySwap {
    fn swap(
        &self,
        install_dir: &Path,
        staged_valv: Option<&Path>,
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
        staged_valv: Option<&Path>,
        staged_valvd: Option<&Path>,
    ) -> Result<SwapBackup> {
        let (valv_current, valv_backup) = match staged_valv {
            Some(staged_valv) => {
                let valv_current = install_dir.join("valv");
                let valv_backup = install_dir.join("valv.old");

                rename_atomic(&valv_current, &valv_backup)?;

                if let Err(error) = rename_atomic(staged_valv, &valv_current) {
                    restore_binary(&valv_backup, &valv_current);
                    return Err(error.context("failed to install new valv binary"));
                }

                if let Err(error) = set_executable(&valv_current) {
                    restore_binary(&valv_backup, &valv_current);
                    return Err(error.context("failed to set executable permission on new valv binary"));
                }

                (Some(valv_current), Some(valv_backup))
            }
            None => (None, None),
        };

        let (valvd_current, valvd_backup) = match staged_valvd {
            Some(staged_valvd) => {
                let valvd_current = install_dir.join("valvd");
                let valvd_backup = install_dir.join("valvd.old");

                if let Err(error) = rename_atomic(&valvd_current, &valvd_backup) {
                    rollback_optional(&valv_backup, &valv_current);
                    return Err(error.context("failed to back up valvd binary"));
                }

                if let Err(error) = rename_atomic(staged_valvd, &valvd_current) {
                    restore_binary(&valvd_backup, &valvd_current);
                    rollback_optional(&valv_backup, &valv_current);
                    return Err(error.context("failed to install new valvd binary"));
                }

                if let Err(error) = set_executable(&valvd_current) {
                    restore_binary(&valvd_backup, &valvd_current);
                    rollback_optional(&valv_backup, &valv_current);
                    return Err(
                        error.context("failed to set executable permission on new valvd binary")
                    );
                }

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
        if let (Some(valv_current), Some(valv_backup)) = (&backup.valv_current, &backup.valv_backup) {
            rename_atomic(valv_backup, valv_current).context("failed to roll back valv binary")?;
        }
        if let (Some(valvd_current), Some(valvd_backup)) =
            (&backup.valvd_current, &backup.valvd_backup)
        {
            rename_atomic(valvd_backup, valvd_current)
                .context("failed to roll back valvd binary")?;
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
fn restore_binary(backup: &Path, current: &Path) {
    let _ = rename_atomic(backup, current);
}

#[cfg(unix)]
fn rollback_optional(backup: &Option<PathBuf>, current: &Option<PathBuf>) {
    if let (Some(backup), Some(current)) = (backup, current) {
        restore_binary(backup, current);
    }
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
        _staged_valv: Option<&Path>,
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

async fn current_daemon_version() -> Option<String> {
    let client = daemon_client().ok()?;
    let response = client.get("http://localhost/status").send().await.ok()?;
    let status = response.json::<DaemonStatus>().await.ok()?;
    Some(status.version)
}

fn installed_binary_version(binary: &Path) -> Option<String> {
    let output = ProcessCommand::new(binary).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version_output(&String::from_utf8(output.stdout).ok()?)
}

fn parse_version_output(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            let parts: Vec<&str> = token.split('.').collect();
            parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use valv_sync::protocol::ipc::Credential;

    #[test]
    fn parse_version_output_extracts_semver_from_binary_output() {
        assert_eq!(parse_version_output("valvd 0.2.2\n").as_deref(), Some("0.2.2"));
        assert_eq!(parse_version_output("valvd v1.10.3").as_deref(), Some("1.10.3"));
        assert_eq!(parse_version_output("").as_deref(), None);
        assert_eq!(parse_version_output("no version here").as_deref(), None);
    }

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
    fn plan_update_installs_when_newer() {
        assert_eq!(plan_update("0.1.0", "0.2.0", false), UpdatePlan::Install);
    }

    #[test]
    fn plan_update_pin_installs_regardless_of_version_comparison() {
        assert_eq!(plan_update("0.2.0", "0.2.0", true), UpdatePlan::Install);
        assert_eq!(plan_update("0.2.0", "0.1.0", true), UpdatePlan::Install);
    }

    #[test]
    fn env_pin_is_set_requires_a_non_empty_value() {
        std::env::set_var("VALV_UPDATE_TEST_PIN_SET", "0.1.0");
        std::env::set_var("VALV_UPDATE_TEST_PIN_EMPTY", "");
        std::env::remove_var("VALV_UPDATE_TEST_PIN_UNSET");

        assert!(env_pin_is_set("VALV_UPDATE_TEST_PIN_SET"));
        assert!(!env_pin_is_set("VALV_UPDATE_TEST_PIN_EMPTY"));
        assert!(!env_pin_is_set("VALV_UPDATE_TEST_PIN_UNSET"));

        std::env::remove_var("VALV_UPDATE_TEST_PIN_SET");
        std::env::remove_var("VALV_UPDATE_TEST_PIN_EMPTY");
    }

    #[test]
    fn component_asset_name_uses_binary_prefix() {
        assert_eq!(
            component_asset_name("valv", "0.3.1", "aarch64-apple-darwin"),
            "valv-0.3.1-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            component_asset_name("valvd", "0.3.1", "x86_64-unknown-linux-gnu"),
            "valvd-0.3.1-x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    #[test]
    fn component_release_base_uses_tag_prefix() {
        assert_eq!(
            component_release_base("DRNKNDev/valv", Component::Cli, "0.3.1"),
            "https://github.com/DRNKNDev/valv/releases/download/cli-v0.3.1"
        );
        assert_eq!(
            component_release_base("DRNKNDev/valv", Component::Valvd, "0.3.1"),
            "https://github.com/DRNKNDev/valv/releases/download/valvd-v0.3.1"
        );
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
            .swap(install_dir, Some(&staged_valv), Some(staged_valvd.as_path()))
            .unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"new valv");
        assert_eq!(fs::read(install_dir.join("valvd")).unwrap(), b"new valvd");
        assert_eq!(fs::read(install_dir.join("valv.old")).unwrap(), b"old valv");

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
        let backup = swap.swap(install_dir, Some(&staged_valv), None).unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"new valv");
        assert!(!install_dir.join("valvd").exists());
        assert!(backup.valvd_current.is_none());
        assert!(backup.valvd_backup.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn unix_binary_swap_skips_valv_when_only_valvd_staged() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path();
        fs::write(install_dir.join("valv"), b"old valv").unwrap();
        fs::write(install_dir.join("valvd"), b"old valvd").unwrap();
        let staged_valvd = install_dir.join("staged-valvd");
        fs::write(&staged_valvd, b"new valvd").unwrap();

        let swap = UnixBinarySwap;
        let backup = swap.swap(install_dir, None, Some(&staged_valvd)).unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"old valv");
        assert_eq!(fs::read(install_dir.join("valvd")).unwrap(), b"new valvd");
        assert!(backup.valv_current.is_none());
        assert!(backup.valv_backup.is_none());
        assert!(!install_dir.join("valv.old").exists());

        swap.rollback(&backup).unwrap();

        assert_eq!(fs::read(install_dir.join("valv")).unwrap(), b"old valv");
        assert_eq!(fs::read(install_dir.join("valvd")).unwrap(), b"old valvd");
    }

    #[cfg(unix)]
    #[test]
    fn unix_binary_swap_rolls_back_valv_when_valvd_backup_step_fails() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path();
        fs::write(install_dir.join("valv"), b"old valv").unwrap();
        let staged_valv = install_dir.join("staged-valv");
        let staged_valvd = install_dir.join("staged-valvd");
        fs::write(&staged_valv, b"new valv").unwrap();
        fs::write(&staged_valvd, b"new valvd").unwrap();

        let swap = UnixBinarySwap;
        let result = swap.swap(install_dir, Some(&staged_valv), Some(staged_valvd.as_path()));

        assert!(result.is_err());
        assert_eq!(
            fs::read(install_dir.join("valv")).unwrap(),
            b"old valv",
            "valv must be restored to its prior bytes"
        );
        assert!(
            !install_dir.join("valv.old").exists(),
            "no leftover valv.old backup after rollback"
        );
        assert!(
            !install_dir.join("valvd").exists(),
            "no half-installed valvd"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_binary_swap_rolls_back_both_when_valvd_install_step_fails() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path();
        fs::write(install_dir.join("valv"), b"old valv").unwrap();
        fs::write(install_dir.join("valvd"), b"old valvd").unwrap();
        let staged_valv = install_dir.join("staged-valv");
        fs::write(&staged_valv, b"new valv").unwrap();
        let missing_staged_valvd = install_dir.join("staged-valvd-missing");

        let swap = UnixBinarySwap;
        let result = swap.swap(
            install_dir,
            Some(&staged_valv),
            Some(missing_staged_valvd.as_path()),
        );

        assert!(result.is_err());
        assert_eq!(
            fs::read(install_dir.join("valv")).unwrap(),
            b"old valv",
            "valv must be restored to its prior bytes"
        );
        assert_eq!(
            fs::read(install_dir.join("valvd")).unwrap(),
            b"old valvd",
            "valvd must be restored to its prior bytes"
        );
        assert!(!install_dir.join("valv.old").exists());
        assert!(!install_dir.join("valvd.old").exists());
    }

    #[cfg(unix)]
    #[test]
    fn handle_failed_restart_rolls_back_and_restarts_then_reports() {
        use std::sync::atomic::{AtomicBool, Ordering};

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
            .swap(install_dir, Some(&staged_valv), Some(staged_valvd.as_path()))
            .unwrap();

        let restarted = AtomicBool::new(false);
        let error = handle_failed_restart(
            &swap,
            &backup,
            || {
                restarted.store(true, Ordering::SeqCst);
                Ok(())
            },
            anyhow!("daemon did not report the new version"),
        );

        assert!(restarted.load(Ordering::SeqCst), "daemon must be restarted");
        assert_eq!(
            fs::read(install_dir.join("valv")).unwrap(),
            b"old valv",
            "valv rolled back to prior bytes"
        );
        assert_eq!(
            fs::read(install_dir.join("valvd")).unwrap(),
            b"old valvd",
            "valvd rolled back to prior bytes"
        );
        assert!(error.to_string().contains("rolled back"));
    }

    #[cfg(unix)]
    #[test]
    fn handle_failed_restart_reports_when_rollback_also_fails() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path();
        let backup = SwapBackup {
            valv_current: Some(install_dir.join("valv")),
            valv_backup: Some(install_dir.join("valv.old-missing")),
            valvd_current: None,
            valvd_backup: None,
        };

        let restarted = AtomicBool::new(false);
        let error = handle_failed_restart(
            &UnixBinarySwap,
            &backup,
            || {
                restarted.store(true, Ordering::SeqCst);
                Ok(())
            },
            anyhow!("daemon did not report the new version"),
        );

        assert!(
            restarted.load(Ordering::SeqCst),
            "daemon restart is still attempted even when rollback failed"
        );
        assert!(
            error.to_string().contains("rollback also failed"),
            "rollback failure must not be swallowed: {error}"
        );
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
            credential: Credential::None,
            principal: None,
        };
        assert!(daemon_status_matches_version(&status, "0.2.0"));
        assert!(!daemon_status_matches_version(&status, "0.1.0"));
    }
}
