use std::{fs, process::Command as ProcessCommand, time::Duration};

use anyhow::{anyhow, Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::StatusCode;
use valv_sync::protocol::ipc::{
    DaemonStatus, MountRequest, MountResponse, RestoreRequest, RestoreResponse, SyncRequest,
    SyncSummary, UnmountRequest, VersionsRequest, VersionsResponse,
};

use crate::{
    auth::{cmd_auth_login, default_auth_login_args},
    daemon::{daemon_client, expect_status, map_daemon_error, parse_daemon_json},
    grants::{cmd_grant_create, cmd_grant_revoke, cmd_grants},
    paths::resolve_valvd_path,
    table::print_table,
    update::cmd_update,
    update_notice::maybe_print_update_notice,
};

#[derive(Parser)]
#[command(name = "valv", about = "Valv sync CLI", version)]
struct Cli {
    /// Print machine-readable JSON instead of a human-formatted table; supported on status, versions, and grants.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in this device and write the local CLI configuration needed by Valv.
    Auth {
        /// Authentication workflow to run.
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Mount an existing Valv folder, accept a grant token, or create a new shared folder from a local path.
    Mount {
        /// Local directory to materialize and keep in sync, such as ~/Documents/project.
        path: String,
        /// Existing folder id to mount at the local path instead of creating a new folder.
        #[arg(long)]
        folder: Option<String>,
        /// One-time grant token from another device to mount a shared folder.
        #[arg(long)]
        grant: Option<String>,
    },
    /// Unmounts locally only - does not delete the shared folder or its grants, and
    /// does not remove the locally materialized files.
    Unmount {
        /// Folder id to unmount from this device.
        #[arg(long)]
        folder: String,
    },
    /// Show daemon connectivity, pause state, and every mounted folder's sync status.
    Status,
    /// Pause background filesystem watching and sync work for this device.
    Pause,
    /// Resume background filesystem watching and sync work for this device.
    Resume,
    /// Ask the daemon to run a sync pass now, optionally limited to one folder id.
    Sync {
        /// Folder id to sync; omit to sync all mounted folders.
        #[arg(long)]
        folder: Option<String>,
    },
    /// List stored versions for a local file inside a mounted folder.
    Versions {
        /// Local file path whose version history should be listed.
        path: String,
    },
    /// Restore a local file to a specific stored version.
    Restore {
        /// Local file path to restore.
        path: String,
        /// Version id from `valv versions <path>` to restore.
        version_id: String,
    },
    /// Create or revoke access grants for mounted folders.
    Grant {
        /// Grant-management action to run.
        #[command(subcommand)]
        command: GrantCommand,
    },
    /// List grants for a mounted folder, or for the first mounted folder if no path is supplied.
    Grants {
        /// Optional local path inside the mounted folder whose grants should be listed.
        folder_path: Option<String>,
    },
    /// Install or uninstall the background daemon service by delegating to `valvd`.
    Daemon {
        /// Daemon service action to run.
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Resolve, verify, and install the latest released valv/valvd, unless --check is passed.
    Update {
        /// Report whether a newer version is available without downloading or installing anything.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Complete browser-based login and save the backend URL and device token locally.
    Login {
        /// Web app base URL that opens for login; defaults to the built-in Valv URL.
        #[arg(long)]
        web_base_url: Option<String>,
        /// Backend API URL to store in config.toml after login.
        #[arg(long)]
        backend_url: Option<String>,
        /// Human-readable name for this device in grants and sync metadata.
        #[arg(long)]
        device_name: Option<String>,
        /// Print the login URL instead of opening it in the default browser.
        #[arg(long)]
        no_open: bool,
    },
}

#[derive(Subcommand)]
enum GrantCommand {
    /// Create an invite link or a one-time device token for access to a mounted folder path.
    Create(GrantCreateArgs),
    /// Revoke an existing grant so the grantee can no longer access the shared scope.
    Revoke {
        /// Grant id shown by `valv grants`.
        grant_id: String,
    },
}

#[derive(Parser)]
#[command(group(ArgGroup::new("target").required(true).args(["to", "device"])))]
#[command(group(ArgGroup::new("access").args(["write", "read_only"])))]
pub(crate) struct GrantCreateArgs {
    /// Local file or folder path that defines the grant scope.
    pub(crate) node_path: String,
    /// Email address to invite through the backend, producing an invite URL.
    #[arg(long)]
    pub(crate) to: Option<String>,
    /// Device name to provision directly, producing a one-time mount token.
    #[arg(long)]
    pub(crate) device: Option<String>,
    /// Allow the grantee to upload changes as well as read files.
    #[arg(long)]
    pub(crate) write: bool,
    /// Limit the grantee to read-only access.
    #[arg(long = "read-only")]
    pub(crate) read_only: bool,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Install and start the Valv daemon service for the current user.
    Install,
    /// Stop and remove the Valv daemon service for the current user.
    Uninstall,
}

pub(crate) async fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;
    let is_update_command = matches!(cli.command, Command::Update { .. });
    let result = run_command(cli.command, json).await;
    // Print update notices only after successful non-update commands.
    if result.is_ok() && !is_update_command {
        maybe_print_update_notice().await;
    }
    result
}

async fn run_command(command: Command, json: bool) -> Result<()> {
    match command {
        Command::Auth { command } => match command {
            AuthCommand::Login {
                web_base_url,
                backend_url,
                device_name,
                no_open,
            } => {
                let mut args = default_auth_login_args(!no_open);
                if let Some(web_base_url) = web_base_url {
                    args.web_base_url = web_base_url;
                }
                if let Some(backend_url) = backend_url {
                    args.backend_url = backend_url;
                }
                if let Some(device_name) = device_name {
                    args.device_name = device_name;
                }
                cmd_auth_login(args).await
            }
        },
        Command::Mount {
            path,
            folder,
            grant,
        } => cmd_mount(path, folder, grant).await,
        Command::Unmount { folder } => cmd_unmount(folder).await,
        Command::Status => cmd_status(json).await,
        Command::Pause => cmd_pause_resume("pause", "Paused sync: background sync is paused").await,
        Command::Resume => {
            cmd_pause_resume("resume", "Resumed sync: background sync is running").await
        }
        Command::Sync { folder } => cmd_sync(folder).await,
        Command::Versions { path } => cmd_versions(path, json).await,
        Command::Restore { path, version_id } => cmd_restore(path, version_id).await,
        Command::Grant { command } => match command {
            GrantCommand::Create(args) => cmd_grant_create(args).await,
            GrantCommand::Revoke { grant_id } => cmd_grant_revoke(grant_id).await,
        },
        Command::Grants { folder_path } => cmd_grants(folder_path, json).await,
        Command::Daemon { command } => delegate_daemon(command),
        Command::Update { check } => cmd_update(check).await,
    }
}

async fn cmd_mount(path: String, folder: Option<String>, grant: Option<String>) -> Result<()> {
    if folder.is_some() && grant.is_some() {
        return Err(anyhow!("--folder and --grant are mutually exclusive"));
    }
    let spinner = request_spinner("Mounting…");
    let mounted = async {
        let response = daemon_client()
            .context("failed to create daemon client for mount")?
            .post("http://localhost/mount")
            .json(&MountRequest {
                path: path.clone(),
                folder_id: folder.clone(),
                grant_token: grant,
            })
            .send()
            .await
            .map_err(|error| daemon_request_error("mount", error))?;
        parse_daemon_json::<MountResponse>(response).await
    }
    .await;
    spinner.finish_and_clear();
    let mounted = mounted?;
    if folder.is_some() {
        println!("Mounted folder {}: {}", mounted.folder_id, mounted.path);
    } else {
        println!("Mounted new folder {}: {}", mounted.folder_id, mounted.path);
    }
    Ok(())
}

fn request_spinner(message: &'static str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}").expect("spinner template is valid"),
    );
    spinner.set_message(message);
    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner
}

fn daemon_request_error(action: &str, error: reqwest::Error) -> anyhow::Error {
    let error = map_daemon_error(error);
    anyhow!("failed to send {action} request to daemon: {error}")
}

async fn cmd_unmount(folder: String) -> Result<()> {
    let response = daemon_client()
        .context("failed to create daemon client for unmount")?
        .delete("http://localhost/mount")
        .json(&UnmountRequest {
            folder_id: folder.clone(),
        })
        .send()
        .await
        .map_err(|error| daemon_request_error("unmount", error))?;
    expect_status(response, StatusCode::NO_CONTENT).await?;
    println!("Unmounted folder {folder}");
    Ok(())
}

async fn cmd_versions(path: String, json: bool) -> Result<()> {
    let local_path = canonical_path(&path)?;
    let response = daemon_client()
        .context("failed to create daemon client for versions")?
        .post("http://localhost/versions")
        .json(&VersionsRequest { local_path })
        .send()
        .await
        .map_err(|error| daemon_request_error("versions", error))?;
    let response = parse_daemon_json::<VersionsResponse>(response).await?;
    if json {
        println!("{}", versions_json(&response)?);
        return Ok(());
    }
    let rows = response
        .versions
        .into_iter()
        .map(|version| {
            vec![
                version.version_id,
                version.created_at,
                version.size_bytes.to_string(),
                version.author_device_name,
                if version.is_conflict_copy {
                    "yes".into()
                } else {
                    "no".into()
                },
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "version_id",
            "created_at",
            "size_bytes",
            "author_device",
            "conflict_copy",
        ],
        &rows,
    );
    Ok(())
}

async fn cmd_restore(path: String, version_id: String) -> Result<()> {
    let local_path = canonical_path(&path)?;
    let response = daemon_client()
        .context("failed to create daemon client for restore")?
        .post("http://localhost/restore")
        .json(&RestoreRequest {
            local_path: local_path.clone(),
            version_id: version_id.clone(),
        })
        .send()
        .await
        .map_err(|error| daemon_request_error("restore", error))?;
    let response = parse_daemon_json::<RestoreResponse>(response).await?;
    match response.result.as_str() {
        "applied" => println!("Restored {local_path} to version {version_id}"),
        "conflict_copy" => {
            println!("Restored as conflict copy — another write occurred concurrently")
        }
        "superseded" => {
            println!("Restore superseded — a concurrent write already advanced the file")
        }
        result => println!("Restore result: {result}"),
    }
    Ok(())
}

fn canonical_path(path: &str) -> Result<String> {
    Ok(fs::canonicalize(path)
        .map_err(|error| anyhow!("failed to resolve {path}: {error}"))?
        .to_string_lossy()
        .into_owned())
}

async fn cmd_status(json: bool) -> Result<()> {
    let response = daemon_client()
        .context("failed to create daemon client for status")?
        .get("http://localhost/status")
        .send()
        .await
        .map_err(|error| daemon_request_error("status", error))?;
    let status = parse_daemon_json::<DaemonStatus>(response).await?;
    if json {
        println!("{}", status_json(&status)?);
        return Ok(());
    }
    if status.update_required {
        println!("{UPDATE_REQUIRED_MESSAGE}");
    } else if status.paused {
        println!("Paused");
    } else if status.backend_connected {
        println!("Connected");
    } else {
        println!("Disconnected");
    }
    if let Some(line) =
        update_available_line(status.update_available, status.latest_version.as_deref())
    {
        println!("{line}");
    }
    let rows = status
        .mounts
        .into_iter()
        .map(|mount| {
            vec![
                mount.path,
                mount.syncing.to_string(),
                mount.pending_ops.to_string(),
                mount.last_synced_at.unwrap_or_else(|| "-".into()),
                update_required_cell(mount.update_required),
                mount.error.unwrap_or_else(|| "-".into()),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "path",
            "syncing",
            "pending_ops",
            "last_synced_at",
            "update_required",
            "error",
        ],
        &rows,
    );
    Ok(())
}

const UPDATE_REQUIRED_MESSAGE: &str = "Update required — run 'valv update' to fix this";

fn update_required_cell(update_required: bool) -> String {
    if update_required {
        UPDATE_REQUIRED_MESSAGE.to_owned()
    } else {
        "false".to_owned()
    }
}

fn update_available_line(update_available: Option<bool>, latest_version: Option<&str>) -> Option<String> {
    match (update_available, latest_version) {
        (Some(true), Some(latest_version)) => Some(format!(
            "A newer version of valv is available ({latest_version}). Run 'valv update' to install it."
        )),
        _ => None,
    }
}

fn status_json(status: &DaemonStatus) -> Result<String> {
    serde_json::to_string(status).context("failed to serialize status as JSON")
}

fn versions_json(response: &VersionsResponse) -> Result<String> {
    serde_json::to_string(&response.versions).context("failed to serialize versions as JSON")
}

async fn cmd_pause_resume(route: &str, message: &str) -> Result<()> {
    let response = daemon_client()
        .context("failed to create daemon client for pause/resume")?
        .post(format!("http://localhost/{route}"))
        .send()
        .await
        .map_err(|error| daemon_request_error(route, error))?;
    expect_status(response, StatusCode::NO_CONTENT).await?;
    println!("{message}");
    Ok(())
}

async fn cmd_sync(folder: Option<String>) -> Result<()> {
    let subject = folder
        .as_ref()
        .map(|folder_id| format!("folder {folder_id}"))
        .unwrap_or_else(|| "folders".into());
    let spinner = request_spinner("Syncing…");
    let summary = async {
        let response = daemon_client()
            .context("failed to create daemon client for sync")?
            .post("http://localhost/sync")
            .json(&SyncRequest { folder_id: folder })
            .send()
            .await
            .map_err(|error| daemon_request_error("sync", error))?;
        parse_daemon_json::<SyncSummary>(response).await
    }
    .await;
    spinner.finish_and_clear();
    let summary = summary?;
    println!(
        "Synced {subject}: {} created, {} updated, {} deleted, {} remote ops applied",
        summary.creates_submitted,
        summary.versions_submitted,
        summary.deletes_submitted,
        summary.pulled_ops
    );
    Ok(())
}

fn delegate_daemon(command: DaemonCommand) -> Result<()> {
    let valvd = resolve_valvd_path().context("failed to resolve valvd path")?;
    let subcommand = match command {
        DaemonCommand::Install => "install",
        DaemonCommand::Uninstall => "uninstall",
    };
    let status = ProcessCommand::new(valvd)
        .arg("daemon")
        .arg(subcommand)
        .status()
        .context("failed to launch valvd")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use valv_sync::protocol::ipc::{Credential, MountStatus, VersionEntry};

    #[test]
    fn status_json_round_trips_without_human_table_text() {
        let status = DaemonStatus {
            paused: false,
            backend_connected: true,
            version: "0.1.0".into(),
            update_required: false,
            mounts: vec![MountStatus {
                path: "/tmp/valv".into(),
                folder_id: "folder-1".into(),
                name: "Valv".into(),
                scope_node_id: None,
                grant_id: None,
                can_write: true,
                syncing: false,
                pending_ops: 0,
                last_synced_at: None,
                update_required: false,
                error: None,
            }],
            account: None,
            latest_version: None,
            update_available: None,
            credential: Credential::None,
            principal: None,
        };

        let output = status_json(&status).unwrap();
        let parsed: DaemonStatus = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed, status);
        assert!(!output.contains("path syncing"));
        assert!(!output.contains("Connected"));
    }

    #[test]
    fn update_required_cell_names_the_fix_when_true() {
        assert_eq!(
            update_required_cell(true),
            "Update required — run 'valv update' to fix this"
        );
    }

    #[test]
    fn update_required_cell_is_a_bare_false_otherwise() {
        assert_eq!(update_required_cell(false), "false");
    }

    #[test]
    fn update_available_line_names_the_version_when_available() {
        assert_eq!(
            update_available_line(Some(true), Some("0.3.0")).as_deref(),
            Some("A newer version of valv is available (0.3.0). Run 'valv update' to install it.")
        );
    }

    #[test]
    fn update_available_line_is_none_when_not_available_or_absent() {
        // false, absent (old daemon), and true-without-a-version all print nothing.
        assert_eq!(update_available_line(Some(false), Some("0.3.0")), None);
        assert_eq!(update_available_line(None, None), None);
        assert_eq!(update_available_line(Some(true), None), None);
    }

    #[test]
    fn versions_json_emits_array_without_human_table_text() {
        let response = VersionsResponse {
            versions: vec![VersionEntry {
                version_id: "version-1".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                size_bytes: 42,
                author_device_name: "Device".into(),
                is_conflict_copy: false,
            }],
        };

        let output = versions_json(&response).unwrap();
        let parsed: Vec<VersionEntry> = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed, response.versions);
        assert!(output.starts_with('['));
        assert!(!output.contains("version_id created_at"));
    }
}
