use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{anyhow, Result};

use crate::config::{self, home_dir};

const LAUNCH_AGENT_LABEL: &str = "dev.drnkn.valvd";

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
    Ok(())
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
    let contents = launch_agent_plist(valvd_path);
    fs::write(plist_path, contents)?;
    Ok(())
}

fn launch_agent_plist(valvd_path: &Path) -> String {
    let valvd_path = xml_escape(&valvd_path.display().to_string());
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
  <string>/tmp/valvd.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/valvd.log</string>
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
        let plist = launch_agent_plist(Path::new("/tmp/valvd"));

        assert!(plist.contains("dev.drnkn.valvd"));
        assert!(plist.contains("<string>/tmp/valvd</string>"));
        assert!(plist.contains("<string>run</string>"));
    }
}
