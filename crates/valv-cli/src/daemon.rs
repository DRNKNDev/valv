use std::{
    fs,
    path::Path,
    process::{Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use valv_sync::protocol::ipc::{Credential, DaemonStatus};

use crate::{
    auth::{default_backend_url, default_device_name, set_owner_only_permissions},
    error::CliError,
    paths,
};

const ENSURE_DAEMON_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn daemon_client() -> Result<reqwest::Client> {
    let path = paths::socket_path().context("failed to determine daemon socket path")?;
    if !path.exists() {
        return Err(CliError::daemon_not_running().into());
    }
    Ok(reqwest::Client::builder()
        .unix_socket(path)
        .build()
        .context("failed to create daemon HTTP client")?)
}

pub(crate) async fn parse_daemon_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T> {
    if !response.status().is_success() {
        return Err(anyhow!(daemon_error_message(response).await));
    }
    Ok(response
        .json::<T>()
        .await
        .context("failed to parse daemon response")?)
}

pub(crate) async fn expect_status(response: reqwest::Response, expected: StatusCode) -> Result<()> {
    if response.status() == expected {
        return Ok(());
    }
    Err(anyhow!(daemon_error_message(response).await))
}

async fn daemon_error_message(response: reqwest::Response) -> String {
    let status = response.status();
    match response.text().await {
        Ok(text) if !text.trim().is_empty() => format!("daemon returned {status}: {text}"),
        _ => format!("daemon returned {status}"),
    }
}

pub(crate) fn map_daemon_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_connect() {
        CliError::daemon_not_running().into()
    } else {
        error.into()
    }
}

pub(crate) async fn probe_status(timeout: Duration) -> Option<DaemonStatus> {
    let path = paths::socket_path().ok()?;
    if !path.exists() {
        return None;
    }
    let client = reqwest::Client::builder()
        .unix_socket(path)
        .timeout(timeout)
        .build()
        .ok()?;
    let response = client.get("http://localhost/status").send().await.ok()?;
    response.json::<DaemonStatus>().await.ok()
}

pub(crate) async fn probe_status_default() -> Option<DaemonStatus> {
    probe_status(PROBE_TIMEOUT).await
}

pub(crate) async fn probe_credential() -> Option<Credential> {
    probe_status_default().await.map(|status| status.credential)
}

pub(crate) async fn ensure_daemon(backend_url_override: Option<&str>) -> Result<()> {
    let config_path = paths::config_path().context("failed to determine config path")?;
    write_config_if_absent(&config_path, backend_url_override)
        .context("failed to write the default Valv configuration")?;

    if socket_connects_now() {
        return Ok(());
    }

    run_valvd_install().context("failed to install/start the Valv daemon")?;
    wait_for_daemon_socket(ENSURE_DAEMON_TIMEOUT)?;
    eprintln!("Started the Valv daemon.");
    Ok(())
}

fn write_config_if_absent(path: &Path, backend_url_override: Option<&str>) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backend_url = backend_url_override
        .map(str::to_owned)
        .unwrap_or_else(default_backend_url);
    let device_name = default_device_name();
    let contents = format!(
        "backend_url = \"{}\"\ndevice_name = \"{}\"\n",
        valv_sync::config::toml_escape(&backend_url),
        valv_sync::config::toml_escape(&device_name),
    );
    fs::write(path, contents)?;
    set_owner_only_permissions(path)?;
    Ok(())
}

pub(crate) fn socket_connects_now() -> bool {
    match paths::socket_path() {
        Ok(path) => path.exists() && std::os::unix::net::UnixStream::connect(&path).is_ok(),
        Err(_) => false,
    }
}

pub(crate) fn wait_for_daemon_socket(timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if socket_connects_now() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(CliError::daemon_failed_to_start(daemon_failure_detail()).into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn daemon_failure_detail() -> String {
    let mut message =
        "The Valv daemon did not start serving its socket within the timeout.".to_owned();
    if let Some(tail) = read_last_daemon_error() {
        message.push_str("\n\nLast daemon output:\n");
        message.push_str(&tail);
    }
    message.push_str(&format!("\n\nInspect it with: {}", platform_log_hint()));
    message
}

fn run_valvd_install() -> Result<()> {
    let valvd = paths::resolve_valvd_path().context("failed to resolve valvd path")?;
    run_valvd_install_at(&valvd)
}

fn run_valvd_install_at(valvd: &Path) -> Result<()> {
    let output = ProcessCommand::new(valvd)
        .arg("daemon")
        .arg("install")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            CliError::daemon_failed_to_start(format!(
                "failed to launch {}: {error}",
                valvd.display()
            ))
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(CliError::daemon_failed_to_start(readable_process_failure(&output)).into())
}

fn readable_process_failure(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!(
            "The Valv daemon failed to start. Inspect it with: {}",
            platform_log_hint()
        )
    } else {
        format!("The Valv daemon failed to start.\n{stderr}")
    }
}

pub(crate) enum DaemonAbsenceReason {
    NotConfigured,
    NotInstalled,
    InstalledButFailing { last_error: Option<String> },
}

pub(crate) fn diagnose_daemon_absence() -> DaemonAbsenceReason {
    if !config_file_exists() {
        return DaemonAbsenceReason::NotConfigured;
    }
    if !daemon_service_registered() {
        return DaemonAbsenceReason::NotInstalled;
    }
    DaemonAbsenceReason::InstalledButFailing {
        last_error: read_last_daemon_error(),
    }
}

fn config_file_exists() -> bool {
    paths::config_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn daemon_service_registered() -> bool {
    paths::systemd_unit_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn daemon_service_registered() -> bool {
    paths::launch_agent_plist_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn daemon_service_registered() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn read_last_daemon_error() -> Option<String> {
    let output = ProcessCommand::new("journalctl")
        .args(["--user", "-u", "valvd", "-n", "20", "--no-pager", "-q"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(target_os = "macos")]
fn read_last_daemon_error() -> Option<String> {
    let log_path = paths::daemon_log_path().ok()?;
    let text = fs::read_to_string(log_path).ok()?;
    let tail = text
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    (!tail.is_empty()).then_some(tail)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_last_daemon_error() -> Option<String> {
    None
}

pub(crate) fn platform_log_hint() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "journalctl --user -u valvd -n 50"
    }
    #[cfg(target_os = "macos")]
    {
        "tail -n 50 ~/Library/Logs/Valv/valvd.log"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        "inspect the valvd process output"
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::{HashMap, VecDeque};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) struct MockDaemon {
        routes: HashMap<(&'static str, &'static str), VecDeque<(u16, String)>>,
    }

    impl MockDaemon {
        pub(crate) fn new() -> Self {
            Self {
                routes: HashMap::new(),
            }
        }

        pub(crate) fn route(
            mut self,
            method: &'static str,
            path: &'static str,
            status: u16,
            body: impl Into<String>,
        ) -> Self {
            self.routes
                .entry((method, path))
                .or_default()
                .push_back((status, body.into()));
            self
        }

        pub(crate) fn spawn(self, socket_path: &std::path::Path, request_count: usize) {
            let listener = tokio::net::UnixListener::bind(socket_path)
                .expect("binding the mock daemon socket should succeed in a fresh temp HOME");
            let mut routes = self.routes;
            tokio::spawn(async move {
                for _ in 0..request_count {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let mut buffer = vec![0u8; 8192];
                    let Ok(n) = stream.read(&mut buffer).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
                    let request_line = request.lines().next().unwrap_or_default();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let path = parts.next().unwrap_or_default().to_owned();
                    let (status, body) = routes
                        .iter_mut()
                        .find(|((route_method, route_path), _)| {
                            *route_method == method && *route_path == path
                        })
                        .map(|(_, responses)| {
                            if responses.len() > 1 {
                                responses.pop_front().expect("len > 1")
                            } else {
                                responses
                                    .front()
                                    .cloned()
                                    .unwrap_or((404, "{\"error\":\"unexpected_request\"}".to_owned()))
                            }
                        })
                        .unwrap_or((404, "{\"error\":\"unexpected_request\"}".to_owned()));
                    let status_line = match status {
                        200 => "200 OK",
                        204 => "204 No Content",
                        403 => "403 Forbidden",
                        404 => "404 Not Found",
                        _ => "500 Internal Server Error",
                    };
                    let response = format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn write_config_if_absent_writes_no_credential_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        write_config_if_absent(&path, Some("https://api.example.test")).unwrap();
        let contents = fs::read_to_string(&path).unwrap();

        assert!(contents.contains("backend_url = \"https://api.example.test\""));
        assert!(contents.contains("device_name"));
        assert!(!contents.contains("device_token"));
        assert!(!contents.contains("device_id"));
    }

    #[test]
    fn write_config_if_absent_falls_back_to_the_default_backend_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        write_config_if_absent(&path, None).unwrap();
        let contents = fs::read_to_string(&path).unwrap();

        assert!(contents.contains("backend_url ="));
    }

    #[test]
    fn write_config_if_absent_is_a_no_op_when_config_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "backend_url = \"https://existing.test\"\ndevice_token = \"secret\"\n",
        )
        .unwrap();

        write_config_if_absent(&path, Some("https://overwritten.test")).unwrap();
        let contents = fs::read_to_string(&path).unwrap();

        assert!(contents.contains("https://existing.test"));
        assert!(contents.contains("device_token"));
    }

    #[test]
    fn write_config_if_absent_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        write_config_if_absent(&path, Some("https://api.example.test")).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn wait_for_daemon_socket_succeeds_once_something_serves_it() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let socket_path = paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(&socket_path).unwrap();

        let result = wait_for_daemon_socket(Duration::from_secs(2));

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_ok());
    }

    #[test]
    fn wait_for_daemon_socket_fails_loudly_when_nothing_ever_serves() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let result = wait_for_daemon_socket(Duration::from_millis(300));

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("wait_for_daemon_socket should fail with a CliError");
        assert_eq!(cli_error.payload.code, "daemon_failed_to_start");
        assert_eq!(cli_error.exit_code, 1);
    }

    #[test]
    fn run_valvd_install_at_surfaces_a_daemon_failed_to_start_error_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-valvd");
        fs::write(
            &script,
            "#!/bin/sh\necho 'valvd failed: config not found' 1>&2\nexit 1\n",
        )
        .unwrap();
        set_executable(&script);

        let error = run_valvd_install_at(&script).unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a failing install should surface a CliError");

        assert_eq!(cli_error.payload.code, "daemon_failed_to_start");
        assert!(cli_error
            .payload
            .message
            .contains("valvd failed: config not found"));
    }

    #[test]
    fn run_valvd_install_at_succeeds_on_a_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-valvd");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&script);

        assert!(run_valvd_install_at(&script).is_ok());
    }

    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn daemon_failure_detail_names_the_platform_log_command() {
        let detail = daemon_failure_detail();
        assert!(detail.contains(platform_log_hint()));
    }

    #[test]
    fn diagnose_daemon_absence_reports_not_configured_when_config_toml_is_missing() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let reason = diagnose_daemon_absence();

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(matches!(reason, DaemonAbsenceReason::NotConfigured));
    }

    #[test]
    fn diagnose_daemon_absence_reports_not_installed_when_config_exists_but_no_service_is_registered(
    ) {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let config_path = paths::config_path().unwrap();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "backend_url = \"https://api.example.test\"\n").unwrap();

        let reason = diagnose_daemon_absence();

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(matches!(reason, DaemonAbsenceReason::NotInstalled));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn diagnose_daemon_absence_reports_installed_but_failing_when_the_service_is_registered_but_silent(
    ) {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let config_path = paths::config_path().unwrap();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "backend_url = \"https://api.example.test\"\n").unwrap();
        let plist_path = paths::launch_agent_plist_path().unwrap();
        fs::create_dir_all(plist_path.parent().unwrap()).unwrap();
        fs::write(&plist_path, "<plist/>").unwrap();

        let reason = diagnose_daemon_absence();

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(matches!(
            reason,
            DaemonAbsenceReason::InstalledButFailing { .. }
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn diagnose_daemon_absence_reports_installed_but_failing_when_the_service_is_registered_but_silent(
    ) {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let config_path = paths::config_path().unwrap();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "backend_url = \"https://api.example.test\"\n").unwrap();
        let unit_path = paths::systemd_unit_path().unwrap();
        fs::create_dir_all(unit_path.parent().unwrap()).unwrap();
        fs::write(&unit_path, "[Unit]\n").unwrap();

        let reason = diagnose_daemon_absence();

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(matches!(
            reason,
            DaemonAbsenceReason::InstalledButFailing { .. }
        ));
    }

    #[tokio::test]
    async fn ensure_daemon_returns_silently_when_the_socket_already_serves() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let socket_path = paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(&socket_path).unwrap();

        let result = ensure_daemon(Some("https://api.example.test")).await;

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ensure_daemon_writes_a_credential_less_config_when_absent() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let socket_path = paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(&socket_path).unwrap();

        let result = ensure_daemon(Some("https://api.example.test")).await;
        let config_path = paths::config_path().unwrap();
        let contents = fs::read_to_string(&config_path).unwrap();

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_ok());
        assert!(contents.contains("backend_url = \"https://api.example.test\""));
        assert!(!contents.contains("device_token"));
    }

    #[tokio::test]
    async fn probe_credential_reads_the_daemons_own_classification_not_a_local_guess() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let socket_path = paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer).await;
                let body = r#"{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[],"credential":"access_key","principal":{"type":"access_key","scopes":[]}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let credential = probe_credential().await;

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(credential, Some(Credential::AccessKey));
    }

    #[tokio::test]
    async fn probe_credential_is_none_when_no_daemon_is_reachable() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let credential = probe_credential().await;

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(credential, None);
    }

    #[tokio::test]
    async fn ensure_daemon_fails_loudly_and_never_prints_success_when_the_daemon_never_serves() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let result = ensure_daemon(Some("https://api.example.test")).await;

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a daemon that never starts should fail with a CliError");
        assert_eq!(cli_error.payload.code, "daemon_failed_to_start");
        assert_eq!(cli_error.exit_code, 1);
    }
}
