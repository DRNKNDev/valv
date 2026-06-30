use std::{fs, process::Command as ProcessCommand};

use anyhow::{anyhow, Result};
use clap::{ArgGroup, Parser, Subcommand};
use reqwest::StatusCode;
use valv_sync::protocol::ipc::{
    DaemonStatus, MountRequest, MountResponse, RestoreRequest, RestoreResponse, SyncRequest,
    SyncSummary, VersionsRequest, VersionsResponse,
};

use crate::{
    daemon::{daemon_client, expect_status, map_daemon_error, parse_daemon_json},
    grants::{cmd_grant_create, cmd_grant_revoke, cmd_grants},
    paths::resolve_valvd_path,
};

#[derive(Parser)]
#[command(name = "valv", about = "Valv sync CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Mount {
        path: String,
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        grant: Option<String>,
    },
    Status,
    Pause,
    Resume,
    Sync {
        #[arg(long)]
        folder: Option<String>,
    },
    Versions {
        path: String,
    },
    Restore {
        path: String,
        version_id: String,
    },
    Grant {
        #[command(subcommand)]
        command: GrantCommand,
    },
    Grants {
        folder_path: Option<String>,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand)]
enum GrantCommand {
    Create(GrantCreateArgs),
    Revoke { grant_id: String },
}

#[derive(Parser)]
#[command(group(ArgGroup::new("target").required(true).args(["to", "device"])))]
#[command(group(ArgGroup::new("access").args(["write", "read_only"])))]
pub(crate) struct GrantCreateArgs {
    pub(crate) node_path: String,
    #[arg(long)]
    pub(crate) to: Option<String>,
    #[arg(long)]
    pub(crate) device: Option<String>,
    #[arg(long)]
    pub(crate) write: bool,
    #[arg(long = "read-only")]
    pub(crate) read_only: bool,
}

#[derive(Subcommand)]
enum DaemonCommand {
    Install,
    Uninstall,
}

pub(crate) async fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Mount {
            path,
            folder,
            grant,
        } => cmd_mount(path, folder, grant).await,
        Command::Status => cmd_status().await,
        Command::Pause => cmd_pause_resume("pause", "Sync paused").await,
        Command::Resume => cmd_pause_resume("resume", "Sync resumed").await,
        Command::Sync { folder } => cmd_sync(folder).await,
        Command::Versions { path } => cmd_versions(path).await,
        Command::Restore { path, version_id } => cmd_restore(path, version_id).await,
        Command::Grant { command } => match command {
            GrantCommand::Create(args) => cmd_grant_create(args).await,
            GrantCommand::Revoke { grant_id } => cmd_grant_revoke(grant_id).await,
        },
        Command::Grants { folder_path } => cmd_grants(folder_path).await,
        Command::Daemon { command } => delegate_daemon(command),
    }
}

async fn cmd_mount(path: String, folder: Option<String>, grant: Option<String>) -> Result<()> {
    if folder.is_some() && grant.is_some() {
        return Err(anyhow!("--folder and --grant are mutually exclusive"));
    }
    let response = daemon_client()?
        .post("http://localhost/mount")
        .json(&MountRequest {
            path: path.clone(),
            folder_id: folder.clone(),
            grant_token: grant,
        })
        .send()
        .await
        .map_err(map_daemon_error)?;
    let mounted = parse_daemon_json::<MountResponse>(response).await?;
    if folder.is_some() {
        println!("Mounted folder {} at {}", mounted.folder_id, mounted.path);
    } else {
        println!(
            "Mounted new folder {} at {}",
            mounted.folder_id, mounted.path
        );
    }
    Ok(())
}

async fn cmd_versions(path: String) -> Result<()> {
    let local_path = canonical_path(&path)?;
    let response = daemon_client()?
        .post("http://localhost/versions")
        .json(&VersionsRequest { local_path })
        .send()
        .await
        .map_err(map_daemon_error)?;
    let response = parse_daemon_json::<VersionsResponse>(response).await?;
    for version in response.versions {
        let suffix = if version.is_conflict_copy {
            " (conflict copy)"
        } else {
            ""
        };
        println!(
            "{}\t{}\t{}\t{}{}",
            version.version_id,
            version.created_at,
            version.size_bytes,
            version.author_device_name,
            suffix
        );
    }
    Ok(())
}

async fn cmd_restore(path: String, version_id: String) -> Result<()> {
    let local_path = canonical_path(&path)?;
    let response = daemon_client()?
        .post("http://localhost/restore")
        .json(&RestoreRequest {
            local_path: local_path.clone(),
            version_id: version_id.clone(),
        })
        .send()
        .await
        .map_err(map_daemon_error)?;
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

async fn cmd_status() -> Result<()> {
    let response = daemon_client()?
        .get("http://localhost/status")
        .send()
        .await
        .map_err(map_daemon_error)?;
    let status = parse_daemon_json::<DaemonStatus>(response).await?;
    if status.paused {
        println!("Paused");
    } else if status.backend_connected {
        println!("Connected");
    } else {
        println!("Disconnected");
    }
    println!("path\tsyncing\tpending_ops\tlast_synced_at\terror");
    for mount in status.mounts {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            mount.path,
            mount.syncing,
            mount.pending_ops,
            mount.last_synced_at.unwrap_or_else(|| "-".into()),
            mount.error.unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

async fn cmd_pause_resume(route: &str, message: &str) -> Result<()> {
    let response = daemon_client()?
        .post(format!("http://localhost/{route}"))
        .send()
        .await
        .map_err(map_daemon_error)?;
    expect_status(response, StatusCode::NO_CONTENT).await?;
    println!("{message}");
    Ok(())
}

async fn cmd_sync(folder: Option<String>) -> Result<()> {
    let response = daemon_client()?
        .post("http://localhost/sync")
        .json(&SyncRequest { folder_id: folder })
        .send()
        .await
        .map_err(map_daemon_error)?;
    let summary = parse_daemon_json::<SyncSummary>(response).await?;
    println!(
        "Sync complete: {} created, {} updated, {} remote ops applied",
        summary.creates_submitted, summary.versions_submitted, summary.pulled_ops
    );
    Ok(())
}

fn delegate_daemon(command: DaemonCommand) -> Result<()> {
    let valvd = resolve_valvd_path()?;
    let subcommand = match command {
        DaemonCommand::Install => "install",
        DaemonCommand::Uninstall => "uninstall",
    };
    let status = ProcessCommand::new(valvd)
        .arg("daemon")
        .arg(subcommand)
        .status()?;
    if !status.success() {
        return Err(anyhow!(
            "valvd daemon {subcommand} failed with status {status}"
        ));
    }
    Ok(())
}
