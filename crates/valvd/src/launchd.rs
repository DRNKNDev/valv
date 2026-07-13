use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::Duration,
};

use anyhow::{anyhow, Result};
use valv_sync::protocol::ipc::Credential;

use crate::config::{self, home_dir};

const LAUNCH_AGENT_LABEL: &str = "dev.drnkn.valvd";
const SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn install_daemon() -> Result<()> {
    config::ensure_config_template()?;
    let plist_path = launch_agent_path()?;
    // `bootstrap` refuses to (re)register a Label already loaded in the domain -
    // fails with the generic "Bootstrap failed: 5: Input/output error" regardless
    // of whether the existing job is actually running or crash-looped. Boot out
    // any prior registration first (ignoring failure - "wasn't loaded" is the
    // normal fresh-install case) so install_daemon is safe to re-run as a repair.
    run_launchctl("bootout", &plist_path, false)?;
    let valvd_path = config::resolve_valvd_path()?;
    write_launch_agent_plist(&plist_path, &valvd_path)?;
    run_launchctl("bootstrap", &plist_path, true)?;

    // `RunAtLoad`/`KeepAlive` report a successful load, not a successful
    // serve: a crash-looping daemon still "bootstraps" cleanly.
    if !config::wait_for_daemon_socket(SOCKET_WAIT_TIMEOUT)? {
        return Err(startup_failure());
    }

    println!("Installed valvd launch agent at {}", plist_path.display());
    print_credential_state();
    Ok(())
}

fn print_credential_state() {
    if let Some(Credential::None) = config::fetch_daemon_status().map(|status| status.credential) {
        println!();
        println!("valvd is running, but this machine has no key yet. Next:");
        println!("  valv mount <path> --grant <token>   (headless / access key)");
        println!("  valv auth login                      (sign in to an account)");
    }
}

fn startup_failure() -> anyhow::Error {
    let mut message = "valvd failed to start: launchd reports the launch agent loaded, \
         but the daemon never began serving its socket."
        .to_owned();
    if let Some(tail) = last_stderr_lines() {
        message.push_str("\n\nLast stderr output:\n");
        message.push_str(&tail);
    }
    if let Ok(log_dir) = config::log_dir() {
        message.push_str(&format!(
            "\n\nInspect it at: {}",
            log_dir.join("valvd.stderr.log").display()
        ));
    }
    anyhow!(message)
}

fn last_stderr_lines() -> Option<String> {
    let log_dir = config::log_dir().ok()?;
    let text = fs::read_to_string(log_dir.join("valvd.stderr.log")).ok()?;
    let tail = text
        .lines()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    (!tail.is_empty()).then_some(tail)
}

pub(crate) fn uninstall_daemon() -> Result<()> {
    let plist_path = launch_agent_path()?;
    run_launchctl("bootout", &plist_path, false)?;
    if let Err(error) = fs::remove_file(&plist_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    Ok(())
}

fn launch_agent_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

fn write_launch_agent_plist(plist_path: &Path, valvd_path: &Path) -> Result<()> {
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log_dir = config::log_dir()?;
    fs::create_dir_all(&log_dir)?;
    let contents = launch_agent_plist(
        valvd_path,
        &log_dir.join("valvd.stdout.log"),
        &log_dir.join("valvd.stderr.log"),
    );
    fs::write(plist_path, contents)?;
    Ok(())
}

fn launch_agent_plist(valvd_path: &Path, stdout_path: &Path, stderr_path: &Path) -> String {
    let valvd_path = xml_escape(&valvd_path.display().to_string());
    let stdout_path = xml_escape(&stdout_path.display().to_string());
    let stderr_path = xml_escape(&stderr_path.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{valvd_path}</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#
    )
}

fn run_launchctl(action: &str, plist_path: &Path, fail_on_error: bool) -> Result<()> {
    let domain = launchctl_domain()?;
    let status = ProcessCommand::new("launchctl")
        .arg(action)
        .arg(domain)
        .arg(plist_path)
        .status()?;
    if !status.success() {
        let message = format!("launchctl {action} failed with status {status}");
        if fail_on_error {
            return Err(anyhow!(message));
        }
        eprintln!("{message}");
    }
    Ok(())
}

fn launchctl_domain() -> Result<String> {
    let output = ProcessCommand::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(anyhow!("id -u failed with status {}", output.status));
    }
    let uid = String::from_utf8(output.stdout)?.trim().to_owned();
    Ok(format!("gui/{uid}"))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_plist_contains_label_and_valvd_path() {
        let plist = launch_agent_plist(
            Path::new("/usr/local/bin/valvd"),
            Path::new("/Users/tester/Library/Logs/Valv/valvd.stdout.log"),
            Path::new("/Users/tester/Library/Logs/Valv/valvd.stderr.log"),
        );

        assert!(plist.contains("dev.drnkn.valvd"));
        assert!(plist.contains("<string>/usr/local/bin/valvd</string>"));
        assert!(plist.contains("<string>run</string>"));
    }

    #[test]
    fn launch_agent_plist_uses_user_log_dir_not_tmp() {
        let plist = launch_agent_plist(
            Path::new("/usr/local/bin/valvd"),
            Path::new("/Users/tester/Library/Logs/Valv/valvd.stdout.log"),
            Path::new("/Users/tester/Library/Logs/Valv/valvd.stderr.log"),
        );

        assert!(plist.contains("<string>/Users/tester/Library/Logs/Valv/valvd.stdout.log</string>"));
        assert!(plist.contains("<string>/Users/tester/Library/Logs/Valv/valvd.stderr.log</string>"));
        assert!(!plist.contains("/tmp"));
    }

    #[test]
    fn startup_failure_names_the_log_path_and_never_claims_success() {
        let error = startup_failure();
        let message = error.to_string();

        assert!(message.contains("valvd.stderr.log"));
        assert!(message.contains("failed to start"));
        assert!(!message.to_lowercase().contains("installed"));
    }
}
