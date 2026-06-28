use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{anyhow, Result};

use crate::config::{config_path, default_device_name, home_dir};

const LAUNCH_AGENT_LABEL: &str = "dev.drnkn.valvd";

pub(crate) fn install_daemon() -> Result<()> {
    ensure_config_template()?;
    let plist_path = launch_agent_path()?;
    let valvd_path = resolve_valvd_path()?;
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

fn ensure_config_template() -> Result<()> {
    let path = config_path()?;
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let backend_url = prompt_value("Backend URL", "https://api.valv.dev")?;
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

fn launch_agent_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

fn resolve_valvd_path() -> Result<PathBuf> {
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

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn set_owner_only_permissions(path: &Path) -> Result<()> {
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

    #[test]
    fn launch_agent_plist_contains_label_and_valvd_path() {
        let plist = launch_agent_plist(Path::new("/tmp/valvd"));

        assert!(plist.contains("dev.drnkn.valvd"));
        assert!(plist.contains("<string>/tmp/valvd</string>"));
        assert!(plist.contains("<string>run</string>"));
    }
}
