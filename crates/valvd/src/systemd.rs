use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{anyhow, Result};

use crate::config::{self, home_dir};

const UNIT_NAME: &str = "valvd";

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
    println!("Installed valvd user service at {}", unit_path.display());
    println!("Note: run 'loginctl enable-linger' to start valvd on boot without logging in.");
    Ok(())
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
}
