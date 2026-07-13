use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::Duration,
};

use anyhow::{anyhow, Result};
use valv_sync::protocol::ipc::Credential;

use crate::config::{self, home_dir};

const UNIT_NAME: &str = "valvd";
const SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn install_daemon() -> Result<()> {
    config::ensure_config_template()?;
    let unit_path = unit_path()?;
    let valvd_path = config::resolve_valvd_path()?;
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_unit_file(&unit_path, &valvd_path)?;
    run_systemctl(&["daemon-reload"], false)?;
    run_systemctl(&["enable", "--now", UNIT_NAME], true)?;

    // `Type=simple` reports success the instant the binary is exec'd, so
    // `enable --now` succeeding here proves nothing about the daemon
    // actually serving.
    if !config::wait_for_daemon_socket(SOCKET_WAIT_TIMEOUT)? {
        return Err(startup_failure(&unit_path));
    }

    println!("Installed valvd user service at {}", unit_path.display());
    println!("This is a user unit, not a system unit. Inspect it with:");
    println!("  systemctl --user status valvd");
    println!("  journalctl --user -u valvd -n 50");
    print_credential_state();
    println!();
    println!("Optional, to start valvd on boot without logging in:");
    println!("  loginctl enable-linger {}", current_user());
    Ok(())
}

fn print_credential_state() {
    if let Some(Credential::None) = config::fetch_daemon_status().map(|status| status.credential) {
        println!();
        println!("valvd is running, but this machine has no key yet. Next:");
        println!("  valv mount <path> --key <token>   (headless / access key)");
        println!("  valv login                        (sign in to an account)");
    }
}

fn startup_failure(unit_path: &Path) -> anyhow::Error {
    let mut message = format!(
        "valvd failed to start: systemd reports the user unit at {} enabled, \
         but the daemon never began serving its socket.",
        unit_path.display()
    );
    if let Some(tail) = last_journal_lines() {
        message.push_str("\n\nLast journal output:\n");
        message.push_str(&tail);
    }
    message.push_str("\n\nInspect it with: journalctl --user -u valvd -n 50");
    anyhow!(message)
}

fn last_journal_lines() -> Option<String> {
    let output = ProcessCommand::new("journalctl")
        .args(["--user", "-u", UNIT_NAME, "-n", "5", "--no-pager", "-q"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "$USER".to_owned())
}

pub(crate) fn uninstall_daemon() -> Result<()> {
    run_systemctl(&["disable", "--now", UNIT_NAME], false)?;
    let unit_path = unit_path()?;
    if let Err(error) = fs::remove_file(&unit_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    run_systemctl(&["daemon-reload"], false)?;
    Ok(())
}

fn unit_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".config/systemd/user/valvd.service"))
}

fn write_unit_file(unit_path: &Path, valvd_path: &Path) -> Result<()> {
    fs::write(unit_path, unit_file_content(valvd_path))?;
    Ok(())
}

fn unit_file_content(valvd_path: &Path) -> String {
    let valvd_path = valvd_path.display();
    format!(
        r#"[Unit]
Description=Valv sync daemon
After=network.target

[Service]
Type=simple
ExecStart={valvd_path} run
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#
    )
}

fn run_systemctl(args: &[&str], fail_on_error: bool) -> Result<()> {
    let status = ProcessCommand::new("systemctl")
        .arg("--user")
        .args(args)
        .status()?;
    if !status.success() {
        let message = format!(
            "systemctl --user {} failed with status {status}",
            args.join(" ")
        );
        if fail_on_error {
            return Err(anyhow!(message));
        }
        eprintln!("{message}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_contains_exec_start_and_restart() {
        let unit = unit_file_content(Path::new("/tmp/valvd"));

        assert!(unit.contains("ExecStart=/tmp/valvd run"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5s"));
    }

    #[test]
    fn unit_file_wantedby_default_target() {
        let unit = unit_file_content(Path::new("/tmp/valvd"));

        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn startup_failure_names_the_user_unit_log_command_and_never_claims_success() {
        let error = startup_failure(Path::new("/home/tester/.config/systemd/user/valvd.service"));
        let message = error.to_string();

        assert!(message.contains("journalctl --user -u valvd -n 50"));
        assert!(message.contains("failed to start"));
        assert!(!message.to_lowercase().contains("installed"));
    }
}
