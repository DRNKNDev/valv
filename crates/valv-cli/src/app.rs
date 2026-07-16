use std::{
    collections::HashSet,
    fs,
    io::IsTerminal,
    path::Path,
    process::{Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::StatusCode;
use valv_sync::protocol::ipc::{
    Credential, DaemonStatus, MountRequest, MountResponse, MountStatus, PrincipalStatus,
    RestoreRequest, RestoreResponse, SyncRequest, SyncSummary, UnmountRequest, VersionsRequest,
    VersionsResponse,
};

use crate::{
    auth::{cmd_auth_login, default_auth_login_args},
    config::{load_config, peek_config},
    daemon::{
        daemon_client, diagnose_daemon_absence, ensure_daemon, expect_status, map_daemon_error,
        parse_daemon_json, platform_log_hint, probe_status_default, wait_for_daemon_socket,
        DaemonAbsenceReason,
    },
    error::{confirm, CliError},
    format::{age_from_now, humanize_bytes},
    grants::{
        cmd_share_grant, cmd_share_list, cmd_unshare, fetch_reachable_grants, GrantListEntry,
    },
    paths::{list_local_mounts, resolve_mount, resolve_valvd_path},
    table::print_table,
    update::cmd_update,
    update_notice::maybe_print_update_notice,
};

const SYNC_BARRIER_TIMEOUT: Duration = Duration::from_secs(120);
const SYNC_POLL_INTERVAL: Duration = Duration::from_millis(500);

const EXIT_CODES_HELP: &str = "Exit codes:\n  \
    0   ok\n  \
    1   failed\n  \
    2   usage error\n  \
    75  temporary failure - retry with backoff (daemon starting, backend unreachable)\n  \
    77  refused - this principal may not do this, do not retry";

#[derive(Parser, Debug)]
#[command(
    name = "valv",
    about = "Valv sync CLI",
    version,
    after_help = EXIT_CODES_HELP
)]
struct Cli {
    /// Print machine-readable JSON instead of human-formatted text. Honored by every command.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Sign in this device and write the local CLI configuration needed by Valv.
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
    /// Attach an existing folder (--folder or --key), or create one from this path (--new).
    Mount(MountArgs),
    /// Unmount locally only: does not delete the shared folder, its grants, or the local files.
    Unmount {
        /// Local mount path to unmount.
        path: String,
        /// Skip the confirmation prompt (required in non-interactive sessions).
        #[arg(long)]
        yes: bool,
    },
    /// Show this machine's principal, daemon health, and every mounted folder's sync state.
    Status,
    /// Pause background filesystem watching and sync work for this device.
    Pause,
    /// Ask the daemon to run a sync pass, optionally limited to one local path.
    Sync {
        /// Local path inside a mounted folder to sync; omit to sync every mounted folder.
        path: Option<String>,
    },
    /// Resume background filesystem watching and sync work for this device.
    Resume,
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
    /// Grant access to a folder (--to an email, or --key for a machine), or list who has it.
    Share(ShareArgs),
    /// Revoke a person's or machine's access to a folder.
    Unshare(UnshareArgs),
    /// Restart or uninstall the background daemon service by delegating to `valvd`.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Resolve, verify, and install the latest released valv/valvd.
    Update,
}

#[derive(Parser, Debug)]
#[command(group(ArgGroup::new("source").required(true).args(["folder", "key", "new"])))]
pub(crate) struct MountArgs {
    /// Local directory to materialize and keep in sync, such as ~/Documents/project.
    pub(crate) path: String,
    /// Id or name of a folder this principal can already reach; ids come from `valv status`.
    #[arg(long)]
    pub(crate) folder: Option<String>,
    /// Access key token to redeem, attaching the folder it grants access to.
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) key: Option<String>,
    /// Create a new folder from the contents of `<path>`.
    #[arg(long)]
    pub(crate) new: bool,
}

#[derive(Parser, Debug)]
#[command(group(ArgGroup::new("target").args(["to", "key"])))]
pub(crate) struct ShareArgs {
    /// Local path identifying the folder to share.
    pub(crate) path: String,
    /// Email address to invite; they accept in a browser with their own account.
    #[arg(long)]
    pub(crate) to: Option<String>,
    /// Name for a new access key, issued once and never shown again.
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) key: Option<String>,
    /// Grant read-only access instead of the read/write default.
    #[arg(long = "read-only", requires = "target")]
    pub(crate) read_only: bool,
}

#[derive(Parser, Debug)]
#[command(group(ArgGroup::new("target").required(true).args(["to", "key", "id"])))]
pub(crate) struct UnshareArgs {
    /// Local path identifying the folder to revoke access to.
    pub(crate) path: String,
    /// Revoke the grant or pending invite belonging to this email address.
    #[arg(long)]
    pub(crate) to: Option<String>,
    /// Revoke the access key with this name.
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) key: Option<String>,
    /// Revoke by pinned grant id, printed by `valv share <path>`.
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) id: Option<String>,
    /// Skip the confirmation prompt (required under --json).
    #[arg(long)]
    pub(crate) yes: bool,
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    /// Stop, start, and verify the Valv daemon service is serving before reporting success.
    Restart,
    /// Stop and remove the Valv daemon service for the current user.
    Uninstall,
}

pub(crate) struct AppFailure {
    pub(crate) error: anyhow::Error,
    pub(crate) json: bool,
}

pub(crate) async fn run() -> Result<(), AppFailure> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(clap_error) => {
            use clap::error::ErrorKind;
            if matches!(
                clap_error.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    | ErrorKind::DisplayVersion
            ) {
                let _ = clap_error.print();
                return Ok(());
            }
            let json = json_flag_present();
            return Err(AppFailure {
                error: map_clap_error(clap_error).into(),
                json,
            });
        }
    };
    let json = cli.json;
    let is_update_command = matches!(cli.command, Command::Update);
    let result = run_command(cli.command, json).await;
    // Print update notices only after successful non-update commands.
    if result.is_ok() && !is_update_command {
        maybe_print_update_notice().await;
    }
    result.map_err(|error| AppFailure { error, json })
}

fn json_flag_present() -> bool {
    std::env::args().any(|arg| arg == "--json")
}

fn map_clap_error(error: clap::Error) -> CliError {
    use clap::error::ErrorKind;
    let rendered = error.render().to_string();
    let is_missing_mount_source = error.kind() == ErrorKind::MissingRequiredArgument
        && rendered.contains("--folder")
        && rendered.contains("--key")
        && rendered.contains("--new");
    if is_missing_mount_source {
        return CliError::mount_source_required();
    }
    let is_missing_share_target = error.kind() == ErrorKind::MissingRequiredArgument
        && rendered.contains("--read-only")
        && rendered.contains("--to")
        && rendered.contains("--key");
    if is_missing_share_target {
        return CliError::share_read_only_requires_target();
    }
    CliError::usage("usage_error", rendered.trim_end().to_owned())
}

async fn run_command(command: Command, json: bool) -> Result<()> {
    match command {
        Command::Login {
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
            ensure_daemon(Some(args.backend_url.as_str())).await?;
            cmd_auth_login(args, json).await
        }
        Command::Mount(args) => {
            ensure_daemon(None).await?;
            cmd_mount(args, json).await
        }
        Command::Unmount { path, yes } => {
            ensure_daemon(None).await?;
            cmd_unmount(path, yes, json).await
        }
        Command::Status => cmd_status(json).await,
        Command::Pause => {
            ensure_daemon(None).await?;
            cmd_pause_resume("pause", "Sync paused.", json).await
        }
        Command::Resume => {
            ensure_daemon(None).await?;
            cmd_pause_resume("resume", "Sync resumed.", json).await
        }
        Command::Sync { path } => {
            ensure_daemon(None).await?;
            cmd_sync(path, json).await
        }
        Command::Versions { path } => {
            ensure_daemon(None).await?;
            cmd_versions(path, json).await
        }
        Command::Restore { path, version_id } => {
            ensure_daemon(None).await?;
            cmd_restore(path, version_id, json).await
        }
        Command::Share(args) => {
            if args.to.is_some() || args.key.is_some() {
                cmd_share_grant(args, json).await
            } else {
                cmd_share_list(args.path, json).await
            }
        }
        Command::Unshare(args) => {
            if json && (args.to.is_some() || args.key.is_some()) {
                return Err(CliError::handle_requires_pinned_id().into());
            }
            cmd_unshare(args.path, args.to, args.key, args.id, args.yes, json).await
        }
        Command::Daemon { command } => delegate_daemon(command, json),
        Command::Update => cmd_update(json).await,
    }
}

async fn refuse_if_access_key_cannot_mount(args: &MountArgs) -> Result<()> {
    if args.key.is_some() {
        return Ok(());
    }
    let status = fetch_daemon_status().await?;
    if status.credential != Credential::AccessKey {
        return Ok(());
    }
    if args.new {
        return Err(CliError::access_key_cannot_create_folder().into());
    }
    Err(CliError::access_key_cannot_mount_folder().into())
}

async fn fetch_daemon_status() -> Result<DaemonStatus> {
    let response = daemon_client()
        .context("failed to create daemon client for status")?
        .get("http://localhost/status")
        .send()
        .await
        .map_err(|error| daemon_request_error("status", error))?;
    parse_daemon_json::<DaemonStatus>(response).await
}

fn build_mount_request(args: &MountArgs) -> Result<MountRequest> {
    Ok(MountRequest {
        path: canonical_path(&args.path)?,
        folder_id: args.folder.clone(),
        grant_token: args.key.clone(),
    })
}

async fn cmd_mount(args: MountArgs, json: bool) -> Result<()> {
    refuse_if_access_key_cannot_mount(&args).await?;
    let spinner = request_spinner("Mounting…", json);
    let mounted = async {
        let request = build_mount_request(&args)?;
        let response = daemon_client()
            .context("failed to create daemon client for mount")?
            .post("http://localhost/mount")
            .json(&request)
            .send()
            .await
            .map_err(|error| daemon_request_error("mount", error))?;
        parse_daemon_json::<MountResponse>(response).await
    }
    .await;
    finish_spinner(spinner);
    let mounted = mounted?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "action": if args.new { "created" } else { "attached" },
                "folder_id": mounted.folder_id,
                "path": mounted.path,
            }))?
        );
    } else {
        let name = resolve_mount(&mounted.path)
            .ok()
            .and_then(|mount| mount.name)
            .unwrap_or_else(|| mounted.folder_id.clone());
        println!("{}", mount_success_message(args.new, &name, &mounted.path));
    }
    Ok(())
}

fn mount_success_message(created: bool, name: &str, path: &str) -> String {
    if created {
        format!("Created \"{name}\" and mounted it at {path}.")
    } else {
        format!("Attached \"{name}\" at {path}.")
    }
}

fn request_spinner(message: &'static str, json: bool) -> Option<ProgressBar> {
    if json || !std::io::stdout().is_terminal() {
        return None;
    }
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}").expect("spinner template is valid"),
    );
    spinner.set_message(message);
    spinner.enable_steady_tick(Duration::from_millis(100));
    Some(spinner)
}

fn finish_spinner(spinner: Option<ProgressBar>) {
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
}

fn daemon_request_error(action: &str, error: reqwest::Error) -> anyhow::Error {
    let error = map_daemon_error(error);
    error.context(format!("failed to send {action} request to daemon"))
}

async fn cmd_unmount(path: String, yes: bool, json: bool) -> Result<()> {
    let mount = resolve_mount(&path).with_context(|| format!("failed to resolve mount {path}"))?;
    if mount_holds_last_credential(&mount).await? {
        confirm(
            &format!(
                "Unmounting {path} destroys this machine's only Valv credential. \
                 It cannot be recovered; the folder owner must issue a new one."
            ),
            yes,
        )?;
    }
    let response = daemon_client()
        .context("failed to create daemon client for unmount")?
        .delete("http://localhost/mount")
        .json(&UnmountRequest {
            folder_id: mount.folder_id.clone(),
        })
        .send()
        .await
        .map_err(|error| daemon_request_error("unmount", error))?;
    expect_status(response, StatusCode::NO_CONTENT).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string(
                &serde_json::json!({"folder_id": mount.folder_id, "path": path})
            )?
        );
    } else {
        println!("Unmounted {path}.");
    }
    Ok(())
}

async fn mount_holds_last_credential(
    mount: &valv_sync::persistence::mounts::LocalMount,
) -> Result<bool> {
    if mount.mount_token.is_none() {
        return Ok(false);
    }
    let status = fetch_daemon_status().await?;
    if status.credential == Credential::Account {
        return Ok(false);
    }
    let mounts_holding_a_token = list_local_mounts()
        .unwrap_or_default()
        .iter()
        .filter(|mount| mount.mount_token.is_some())
        .count();
    Ok(mounts_holding_a_token <= 1)
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
        .map(version_row)
        .collect::<Vec<_>>();
    print_table(
        &["VERSION", "CREATED", "SIZE", "AUTHOR", "CONFLICT COPY"],
        &rows,
    );
    Ok(())
}

fn version_row(version: valv_sync::protocol::ipc::VersionEntry) -> Vec<String> {
    let created = age_from_now(&version.created_at).unwrap_or(version.created_at);
    vec![
        version.version_id,
        created,
        humanize_bytes(version.size_bytes),
        version.author_device_name,
        if version.is_conflict_copy {
            "yes".into()
        } else {
            "no".into()
        },
    ]
}

async fn refuse_if_access_key_restore_is_read_only(local_path: &str) -> Result<()> {
    let mount = resolve_mount(local_path)
        .with_context(|| format!("failed to resolve mount for {local_path}"))?;
    if mount.can_write {
        return Ok(());
    }
    let status = fetch_daemon_status().await?;
    if status.credential != Credential::AccessKey {
        return Ok(());
    }
    let folder_label = mount.name.unwrap_or(mount.folder_id);
    Err(CliError::access_key_is_read_only(folder_label).into())
}

async fn cmd_restore(path: String, version_id: String, json: bool) -> Result<()> {
    let local_path = canonical_path(&path)?;
    refuse_if_access_key_restore_is_read_only(&local_path).await?;
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
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "result": response.result,
                "path": local_path,
                "version_id": version_id,
            }))?
        );
        return Ok(());
    }
    match response.result.as_str() {
        "applied" => println!("Restored {local_path} to version {version_id}"),
        "conflict_copy" => println!("{}", conflict_copy_message(&local_path)),
        "superseded" => println!("{}", superseded_message(&local_path)),
        result => println!("Restore result: {result}"),
    }
    Ok(())
}

fn conflict_copy_message(local_path: &str) -> String {
    let parent = Path::new(local_path)
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_else(|| local_path.to_owned());
    format!(
        "Another write happened at the same time, so the restore was saved as a new file in {parent} instead of overwriting {local_path}."
    )
}

fn superseded_message(local_path: &str) -> String {
    format!("{local_path} was not restored: a newer write already happened, so nothing changed.")
}

fn canonical_path(path: &str) -> Result<String> {
    if let Ok(resolved) = fs::canonicalize(path) {
        return Ok(resolved.to_string_lossy().into_owned());
    }
    std::path::absolute(path)
        .map(|absolute| absolute.to_string_lossy().into_owned())
        .with_context(|| format!("failed to resolve {path}"))
}

async fn cmd_status(json: bool) -> Result<()> {
    match probe_status_default().await {
        Some(status) => render_status_online(status, json).await,
        None => render_status_offline(json).await,
    }
}

enum Discovery {
    NotApplicable,
    Unavailable,
    Available(Vec<GrantListEntry>),
}

async fn fetch_reachable_folders(status: &DaemonStatus) -> Discovery {
    if status.credential != Credential::Account {
        return Discovery::NotApplicable;
    }
    let Ok(config) = load_config() else {
        return Discovery::Unavailable;
    };
    match fetch_reachable_grants(&config, Duration::from_secs(3)).await {
        Ok(grants) => Discovery::Available(grants),
        Err(_) => Discovery::Unavailable,
    }
}

async fn render_status_online(status: DaemonStatus, json: bool) -> Result<()> {
    let discovery = fetch_reachable_folders(&status).await;
    if json {
        println!("{}", online_status_json(&status, &discovery)?);
        return Ok(());
    }

    print_principal_line(status.credential, status.principal.as_ref());
    if let Some(advisory) = legacy_device_token_advisory(status.credential) {
        println!("{advisory}");
    }

    if status.paused {
        println!("Paused");
    } else if status.backend_connected {
        println!("Connected");
    } else {
        println!("Disconnected");
    }

    let rows = build_folder_rows(&status, &discovery);
    print_table(&ONLINE_TABLE_HEADERS, &rows);

    for line in mount_state_detail_lines(&status) {
        println!("{line}");
    }

    if matches!(discovery, Discovery::Unavailable) {
        println!("The reachable-folder list is unavailable: the backend could not be reached.");
    }

    if let Some(line) =
        update_available_line(status.update_available, status.latest_version.as_deref())
    {
        println!("{line}");
    }
    if status.update_required {
        println!("{UPDATE_REQUIRED_MESSAGE}");
    }
    Ok(())
}

async fn fetch_offline_reachable_folders() -> Discovery {
    let Ok(config) = load_config() else {
        return Discovery::NotApplicable;
    };
    match fetch_reachable_grants(&config, Duration::from_secs(3)).await {
        Ok(grants) => Discovery::Available(grants),
        Err(_) => Discovery::Unavailable,
    }
}

async fn render_status_offline(json: bool) -> Result<()> {
    let reason = diagnose_daemon_absence();
    let mounts = list_local_mounts().unwrap_or_default();
    let principal_line = offline_principal_line(&mounts);
    let discovery = fetch_offline_reachable_folders().await;

    if json {
        println!(
            "{}",
            offline_status_json(&reason, &principal_line, &mounts, &discovery)?
        );
        return Ok(());
    }

    println!("{principal_line}");
    print_daemon_absence(&reason);

    let rows = mounts
        .iter()
        .map(|mount| {
            vec![
                mount.folder_id.clone(),
                mount.name.clone().unwrap_or_else(|| "-".into()),
                mount.path.clone(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&OFFLINE_TABLE_HEADERS, &rows);

    if let Discovery::Available(grants) = &discovery {
        let mounted_ids = mounts
            .iter()
            .map(|mount| mount.folder_id.as_str())
            .collect::<HashSet<_>>();
        let unmounted_rows = grants
            .iter()
            .filter(|grant| !mounted_ids.contains(grant.folder_id.as_str()))
            .map(|grant| {
                vec![
                    grant.folder_id.clone(),
                    grant.folder_name.clone().unwrap_or_else(|| "-".into()),
                    "not mounted".into(),
                ]
            })
            .collect::<Vec<_>>();
        if !unmounted_rows.is_empty() {
            print_table(&OFFLINE_TABLE_HEADERS, &unmounted_rows);
        }
    }
    if matches!(discovery, Discovery::Unavailable) {
        println!("The reachable-folder list is unavailable: the backend could not be reached.");
    }
    Ok(())
}

fn print_principal_line(credential: Credential, principal: Option<&PrincipalStatus>) {
    for line in principal_lines(credential, principal) {
        println!("{line}");
    }
}

fn principal_lines(credential: Credential, principal: Option<&PrincipalStatus>) -> Vec<String> {
    match credential {
        Credential::Account => {
            let label = principal
                .and_then(|principal| principal.email.clone())
                .unwrap_or_else(offline_device_name_fallback);
            vec![format!("Signed in as {label}")]
        }
        Credential::AccessKey => {
            let mut lines = vec!["Access key".to_owned()];
            for scope in principal
                .map(|principal| principal.scopes.as_slice())
                .unwrap_or_default()
            {
                let permission = if scope.can_write {
                    "read/write"
                } else {
                    "read-only"
                };
                lines.push(format!(
                    "folder-scoped: {} ({permission})",
                    scope.folder_name
                ));
            }
            lines
        }
        Credential::Rejected => {
            vec!["Access key rejected. Ask the folder owner for a new key.".to_owned()]
        }
        Credential::Pending => vec!["Verifying this machine's credential…".to_owned()],
        Credential::None => vec!["Not configured, no key yet".to_owned()],
    }
}

fn legacy_device_token_advisory(credential: Credential) -> Option<&'static str> {
    if credential != Credential::AccessKey {
        return None;
    }
    let has_device_token = peek_config()
        .and_then(|peek| peek.device_token)
        .map(|token| !token.trim().is_empty())
        .unwrap_or(false);
    has_device_token.then_some(
        "This machine's config.toml stores an access key in device_token. Redeem it as a mount instead: valv mount <path> --key <token>, then remove device_token from config.toml.",
    )
}

fn offline_device_name_fallback() -> String {
    peek_config()
        .and_then(|peek| peek.device_name)
        .unwrap_or_else(|| "this device".into())
}

fn access_label(can_write: bool, role: Option<&str>) -> String {
    if role == Some("owner") {
        return "owner".into();
    }
    if can_write {
        "read/write".into()
    } else {
        "read-only".into()
    }
}

fn mount_sync_state(mount: &MountStatus) -> String {
    if mount.update_required {
        return "update required".into();
    }
    if mount.error.is_some() {
        return "error".into();
    }
    if !mount.watcher_alive {
        return "watcher down".into();
    }
    if mount.syncing {
        return "syncing".into();
    }
    if mount.pending_ops > 0 {
        format!("{} pending", mount.pending_ops)
    } else {
        "synced".into()
    }
}

fn build_folder_rows(status: &DaemonStatus, discovery: &Discovery) -> Vec<Vec<String>> {
    let mut rows = status
        .mounts
        .iter()
        .map(|mount| {
            vec![
                mount.folder_id.clone(),
                mount.name.clone(),
                access_label(mount.can_write, None),
                mount_sync_state(mount),
                mount.path.clone(),
            ]
        })
        .collect::<Vec<_>>();

    if let Discovery::Available(grants) = discovery {
        let mounted_ids = status
            .mounts
            .iter()
            .map(|mount| mount.folder_id.as_str())
            .collect::<HashSet<_>>();
        for grant in grants {
            if mounted_ids.contains(grant.folder_id.as_str()) {
                continue;
            }
            rows.push(vec![
                grant.folder_id.clone(),
                grant.folder_name.clone().unwrap_or_else(|| "-".into()),
                access_label(grant.can_write.unwrap_or(false), grant.role.as_deref()),
                "not mounted".into(),
                "-".into(),
            ]);
        }
    }
    rows
}

fn mount_state_detail_lines(status: &DaemonStatus) -> Vec<String> {
    status
        .mounts
        .iter()
        .filter_map(|mount| {
            if mount.update_required {
                Some(format!(
                    "{}: update required, run `valv update` to fix this.",
                    mount.name
                ))
            } else if let Some(error) = &mount.error {
                Some(format!("{}: {error}", mount.name))
            } else {
                None
            }
        })
        .collect()
}

fn offline_principal_line(mounts: &[valv_sync::persistence::mounts::LocalMount]) -> String {
    let Some(peek) = peek_config() else {
        return "Not configured, no key yet".into();
    };
    let has_device_token = peek
        .device_token
        .as_deref()
        .map(|token| !token.trim().is_empty())
        .unwrap_or(false);
    let has_mount_token = mounts.iter().any(|mount| mount.mount_token.is_some());
    if has_device_token || has_mount_token {
        "A local credential is present, but the daemon is unreachable so its type cannot be verified.".into()
    } else {
        "Not configured, no key yet".into()
    }
}

fn print_daemon_absence(reason: &DaemonAbsenceReason) {
    match reason {
        DaemonAbsenceReason::NotConfigured => {
            println!("Daemon: not configured (no config.toml found).");
            println!(
                "Run: valv login, or valv mount <path> --key <token> if you were given an access key."
            );
        }
        DaemonAbsenceReason::NotInstalled => {
            println!("Daemon: not installed (no background service is registered).");
            println!("Run any valv command to install and start it, or run `valv daemon restart`.");
        }
        DaemonAbsenceReason::InstalledButFailing { last_error } => {
            println!("Daemon: installed, but failing to start.");
            if let Some(last_error) = last_error {
                println!("Last daemon output:\n{last_error}");
            }
            println!("Inspect it with: {}", platform_log_hint());
        }
    }
}

const UPDATE_REQUIRED_MESSAGE: &str = "Update required. Run `valv update` to fix this.";

const ONLINE_TABLE_HEADERS: [&str; 5] = ["FOLDER ID", "NAME", "ACCESS", "STATE", "PATH"];
const OFFLINE_TABLE_HEADERS: [&str; 3] = ["FOLDER ID", "NAME", "PATH"];

fn update_available_line(
    update_available: Option<bool>,
    latest_version: Option<&str>,
) -> Option<String> {
    match (update_available, latest_version) {
        (Some(true), Some(latest_version)) => Some(format!(
            "A newer version of valv is available ({latest_version}). Run 'valv update' to install it."
        )),
        _ => None,
    }
}

fn online_status_json(status: &DaemonStatus, discovery: &Discovery) -> Result<String> {
    let mut value = serde_json::to_value(status).context("failed to serialize status as JSON")?;
    if let Some(object) = value.as_object_mut() {
        match discovery {
            Discovery::Available(grants) => {
                let mounted_ids = status
                    .mounts
                    .iter()
                    .map(|mount| mount.folder_id.as_str())
                    .collect::<HashSet<_>>();
                let reachable = grants
                    .iter()
                    .filter(|grant| !mounted_ids.contains(grant.folder_id.as_str()))
                    .map(|grant| {
                        serde_json::json!({
                            "folder_id": grant.folder_id,
                            "name": grant.folder_name,
                            "access": access_label(grant.can_write.unwrap_or(false), grant.role.as_deref()),
                        })
                    })
                    .collect::<Vec<_>>();
                object.insert("reachable_folders".into(), serde_json::json!(reachable));
            }
            Discovery::Unavailable => {
                object.insert("discovery_unavailable".into(), serde_json::json!(true));
            }
            Discovery::NotApplicable => {}
        }
    }
    serde_json::to_string(&value).context("failed to serialize status as JSON")
}

fn offline_status_json(
    reason: &DaemonAbsenceReason,
    principal_summary: &str,
    mounts: &[valv_sync::persistence::mounts::LocalMount],
    discovery: &Discovery,
) -> Result<String> {
    let (state, last_error) = match reason {
        DaemonAbsenceReason::NotConfigured => ("not_configured", None),
        DaemonAbsenceReason::NotInstalled => ("not_installed", None),
        DaemonAbsenceReason::InstalledButFailing { last_error } => {
            ("installed_but_failing", last_error.clone())
        }
    };
    let mount_rows = mounts
        .iter()
        .map(|mount| {
            serde_json::json!({
                "folder_id": mount.folder_id,
                "name": mount.name,
                "path": mount.path,
            })
        })
        .collect::<Vec<_>>();
    let mut value = serde_json::json!({
        "daemon_state": state,
        "last_error": last_error,
        "log_command": if state == "installed_but_failing" { Some(platform_log_hint()) } else { None },
        "principal_summary": principal_summary,
        "mounts": mount_rows,
    });
    if let Some(object) = value.as_object_mut() {
        match discovery {
            Discovery::Available(grants) => {
                let mounted_ids = mounts
                    .iter()
                    .map(|mount| mount.folder_id.as_str())
                    .collect::<HashSet<_>>();
                let reachable = grants
                    .iter()
                    .filter(|grant| !mounted_ids.contains(grant.folder_id.as_str()))
                    .map(|grant| {
                        serde_json::json!({
                            "folder_id": grant.folder_id,
                            "name": grant.folder_name,
                            "access": access_label(grant.can_write.unwrap_or(false), grant.role.as_deref()),
                        })
                    })
                    .collect::<Vec<_>>();
                object.insert("reachable_folders".into(), serde_json::json!(reachable));
            }
            Discovery::Unavailable => {
                object.insert("discovery_unavailable".into(), serde_json::json!(true));
            }
            Discovery::NotApplicable => {}
        }
    }
    serde_json::to_string(&value).context("failed to serialize offline status as JSON")
}

fn versions_json(response: &VersionsResponse) -> Result<String> {
    serde_json::to_string(&response.versions).context("failed to serialize versions as JSON")
}

async fn cmd_pause_resume(route: &str, message: &str, json: bool) -> Result<()> {
    let response = daemon_client()
        .context("failed to create daemon client for pause/resume")?
        .post(format!("http://localhost/{route}"))
        .send()
        .await
        .map_err(|error| daemon_request_error(route, error))?;
    expect_status(response, StatusCode::NO_CONTENT).await?;
    if json {
        println!("{}", serde_json::to_string(&serde_json::json!({ "action": route }))?);
    } else {
        println!("{message}");
    }
    Ok(())
}

async fn request_sync_pass(folder_id: Option<&str>) -> Result<SyncSummary> {
    let response = daemon_client()
        .context("failed to create daemon client for sync")?
        .post("http://localhost/sync")
        .json(&SyncRequest {
            folder_id: folder_id.map(str::to_owned),
        })
        .send()
        .await
        .map_err(|error| daemon_request_error("sync", error))?;
    parse_daemon_json::<SyncSummary>(response).await
}

fn relevant_mounts<'a>(status: &'a DaemonStatus, folder_id: Option<&str>) -> Vec<&'a MountStatus> {
    status
        .mounts
        .iter()
        .filter(|mount| folder_id.is_none_or(|folder_id| mount.folder_id == folder_id))
        .collect()
}

async fn run_sync_barrier(
    folder_id: Option<&str>,
    subject: &str,
    spinner: Option<&ProgressBar>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<SyncSummary> {
    let deadline = Instant::now() + timeout;
    loop {
        let summary = request_sync_pass(folder_id).await?;
        let status = fetch_daemon_status().await?;
        let mounts = relevant_mounts(&status, folder_id);

        if status.backend_connected {
            if let Some(mount) = mounts.iter().find(|mount| mount.error.is_some()) {
                return Err(CliError::sync_mount_error(
                    mount.name.clone(),
                    mount.error.clone().unwrap_or_default(),
                )
                .into());
            }
        }

        let settled = mounts
            .iter()
            .all(|mount| mount.pending_ops == 0 && mount.error.is_none());
        if settled {
            return Ok(summary);
        }

        if Instant::now() >= deadline {
            return Err(
                CliError::sync_timed_out(format!("Timed out waiting for {subject} to settle."))
                    .into(),
            );
        }

        if let Some(spinner) = spinner {
            let pending: u64 = mounts.iter().map(|mount| mount.pending_ops).sum();
            spinner.set_message(format!(
                "Waiting for {subject} to settle: {pending} pending op(s)…"
            ));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn cmd_sync(path: Option<String>, json: bool) -> Result<()> {
    let folder_id = path
        .as_ref()
        .map(|path| resolve_mount(path).map(|mount| mount.folder_id))
        .transpose()?;
    let subject = path
        .clone()
        .unwrap_or_else(|| "every mounted folder".into());
    let spinner = request_spinner("Syncing…", json);

    let result = run_sync_barrier(
        folder_id.as_deref(),
        &subject,
        spinner.as_ref(),
        SYNC_BARRIER_TIMEOUT,
        SYNC_POLL_INTERVAL,
    )
    .await;

    finish_spinner(spinner);
    let summary = result?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "path": path,
                "creates_submitted": summary.creates_submitted,
                "versions_submitted": summary.versions_submitted,
                "deletes_submitted": summary.deletes_submitted,
                "pulled_ops": summary.pulled_ops,
            }))?
        );
        return Ok(());
    }
    println!("{}", sync_summary_line(&subject, &summary));
    Ok(())
}

fn sync_summary_line(subject: &str, summary: &SyncSummary) -> String {
    let mut parts = Vec::new();
    if summary.creates_submitted > 0 {
        parts.push(format!("{} created", summary.creates_submitted));
    }
    if summary.versions_submitted > 0 {
        parts.push(format!("{} updated", summary.versions_submitted));
    }
    if summary.deletes_submitted > 0 {
        parts.push(format!("{} deleted", summary.deletes_submitted));
    }
    if summary.pulled_ops > 0 {
        parts.push(format!("{} changes received", summary.pulled_ops));
    }
    if parts.is_empty() {
        "Already up to date.".to_owned()
    } else {
        format!("Synced {subject}: {}.", parts.join(", "))
    }
}

fn delegate_daemon(command: DaemonCommand, json: bool) -> Result<()> {
    match command {
        DaemonCommand::Restart => cmd_daemon_restart(json),
        DaemonCommand::Uninstall => {
            run_valvd_daemon_subcommand("uninstall", json)?;
            print_daemon_command_result("uninstalled", json);
            Ok(())
        }
    }
}

fn cmd_daemon_restart(json: bool) -> Result<()> {
    let _ = run_valvd_daemon_subcommand("uninstall", json);
    run_valvd_daemon_subcommand("install", json)?;
    wait_for_daemon_socket(Duration::from_secs(10))?;
    print_daemon_command_result("restarted", json);
    Ok(())
}

fn print_daemon_command_result(action: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "action": action }))
                .unwrap_or_else(|_| "{\"action\":\"unknown\"}".to_owned())
        );
    } else {
        eprintln!("Valv daemon {action}.");
    }
}

fn run_valvd_daemon_subcommand(subcommand: &str, json: bool) -> Result<()> {
    let valvd = resolve_valvd_path().context("failed to resolve valvd path")?;
    let mut command = ProcessCommand::new(valvd);
    command.arg("daemon").arg(subcommand);
    if json {
        command.stdout(Stdio::null());
    }
    let status = command.status().context("failed to launch valvd")?;
    if !status.success() {
        return Err(CliError::new(
            1,
            "daemon_command_failed",
            format!("valvd daemon {subcommand} failed"),
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use valv_sync::protocol::ipc::VersionEntry;

    fn sample_status() -> DaemonStatus {
        DaemonStatus {
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
                watcher_alive: true,
            }],
            account: None,
            latest_version: None,
            update_available: None,
            credential: Credential::None,
            principal: None,
        }
    }

    #[test]
    fn online_status_json_round_trips_without_human_table_text_when_discovery_is_not_applicable() {
        let status = sample_status();

        let output = online_status_json(&status, &Discovery::NotApplicable).unwrap();
        let parsed: DaemonStatus = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed, status);
        assert!(!output.contains("path syncing"));
        assert!(!output.contains("Connected"));
    }

    #[test]
    fn online_status_json_adds_reachable_folders_only_for_unmounted_grants() {
        let status = sample_status();
        let grants = vec![
            GrantListEntry {
                grant_id: "g_1".into(),
                folder_id: "folder-1".into(),
                scope_node_id: "root".into(),
                role: Some("owner".into()),
                can_read: Some(true),
                can_write: Some(true),
                user_id: Some("user-1".into()),
                device_id: None,
                name: None,
                grantee_email: None,
                device_name: None,
                created_at: None,
                created_by_email: None,
                folder_name: Some("Valv".into()),
            },
            GrantListEntry {
                grant_id: "g_2".into(),
                folder_id: "folder-2".into(),
                scope_node_id: "root".into(),
                role: Some("collaborator".into()),
                can_read: Some(true),
                can_write: Some(false),
                user_id: Some("user-1".into()),
                device_id: None,
                name: None,
                grantee_email: None,
                device_name: None,
                created_at: None,
                created_by_email: None,
                folder_name: Some("Assets".into()),
            },
        ];

        let output = online_status_json(&status, &Discovery::Available(grants)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        let reachable = value["reachable_folders"].as_array().unwrap();

        assert_eq!(reachable.len(), 1);
        assert_eq!(reachable[0]["folder_id"], "folder-2");
        assert_eq!(reachable[0]["access"], "read-only");
    }

    #[test]
    fn online_status_json_flags_discovery_as_unavailable_rather_than_omitting_it() {
        let status = sample_status();

        let output = online_status_json(&status, &Discovery::Unavailable).unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(value["discovery_unavailable"], true);
    }

    #[test]
    fn principal_lines_account_signs_in_by_email() {
        let principal = PrincipalStatus {
            principal_type: valv_sync::protocol::ipc::PrincipalType::Account,
            email: Some("alice@example.com".into()),
            scopes: vec![],
        };
        assert_eq!(
            principal_lines(Credential::Account, Some(&principal)),
            vec!["Signed in as alice@example.com".to_owned()]
        );
    }

    #[test]
    fn principal_lines_access_key_renders_one_line_per_scope() {
        let principal = PrincipalStatus {
            principal_type: valv_sync::protocol::ipc::PrincipalType::AccessKey,
            email: None,
            scopes: vec![
                valv_sync::protocol::ipc::PrincipalScope {
                    folder_id: "folder-1".into(),
                    folder_name: "Design".into(),
                    scope_label: "Design".into(),
                    can_write: true,
                },
                valv_sync::protocol::ipc::PrincipalScope {
                    folder_id: "folder-2".into(),
                    folder_name: "Assets".into(),
                    scope_label: "Assets".into(),
                    can_write: false,
                },
            ],
        };

        let lines = principal_lines(Credential::AccessKey, Some(&principal));

        assert_eq!(
            lines,
            vec![
                "Access key".to_owned(),
                "folder-scoped: Design (read/write)".to_owned(),
                "folder-scoped: Assets (read-only)".to_owned(),
            ]
        );
    }

    #[test]
    fn principal_lines_rejected_names_the_folder_owner_not_a_network_outage() {
        let lines = principal_lines(Credential::Rejected, None);
        assert_eq!(
            lines,
            vec!["Access key rejected. Ask the folder owner for a new key.".to_owned()]
        );
    }

    #[test]
    fn principal_lines_pending_does_not_guess_account_or_access_key() {
        let lines = principal_lines(Credential::Pending, None);
        assert_eq!(
            lines,
            vec!["Verifying this machine's credential…".to_owned()]
        );
    }

    #[test]
    fn principal_lines_none_reads_not_configured_no_key_yet() {
        let lines = principal_lines(Credential::None, None);
        assert_eq!(lines, vec!["Not configured, no key yet".to_owned()]);
    }

    #[tokio::test]
    async fn fetch_reachable_folders_makes_no_request_for_an_access_key_machine() {
        let mut status = sample_status();
        status.credential = Credential::AccessKey;

        let discovery = fetch_reachable_folders(&status).await;

        assert!(matches!(discovery, Discovery::NotApplicable));
    }

    #[test]
    fn build_folder_rows_still_lists_mounts_when_discovery_is_unavailable() {
        let status = sample_status();

        let rows = build_folder_rows(&status, &Discovery::Unavailable);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], "folder-1");
    }

    #[test]
    fn offline_status_json_adds_reachable_folders_only_for_unmounted_grants() {
        let mounts = vec![valv_sync::persistence::mounts::LocalMount {
            path: "/tmp/valv".into(),
            folder_id: "folder-1".into(),
            grant_id: None,
            scope_node_id: None,
            mount_token: None,
            cursor: 0,
            can_write: true,
            name: Some("Valv".into()),
        }];
        let grants = vec![
            GrantListEntry {
                grant_id: "g_1".into(),
                folder_id: "folder-1".into(),
                scope_node_id: "root".into(),
                role: Some("owner".into()),
                can_read: Some(true),
                can_write: Some(true),
                user_id: Some("user-1".into()),
                device_id: None,
                name: None,
                grantee_email: None,
                device_name: None,
                created_at: None,
                created_by_email: None,
                folder_name: Some("Valv".into()),
            },
            GrantListEntry {
                grant_id: "g_2".into(),
                folder_id: "folder-2".into(),
                scope_node_id: "root".into(),
                role: Some("collaborator".into()),
                can_read: Some(true),
                can_write: Some(false),
                user_id: Some("user-1".into()),
                device_id: None,
                name: None,
                grantee_email: None,
                device_name: None,
                created_at: None,
                created_by_email: None,
                folder_name: Some("Assets".into()),
            },
        ];

        let output = offline_status_json(
            &DaemonAbsenceReason::NotInstalled,
            "Signed in as alice@example.com",
            &mounts,
            &Discovery::Available(grants),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        let reachable = value["reachable_folders"].as_array().unwrap();

        assert_eq!(reachable.len(), 1);
        assert_eq!(reachable[0]["folder_id"], "folder-2");
    }

    #[tokio::test]
    async fn fetch_offline_reachable_folders_calls_grants_when_a_device_token_is_present() {
        let _loopback_guard = crate::LOOPBACK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind loopback test listener: {error}"),
        };

        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer).await.unwrap();
            let body = "[]";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let config_path = crate::paths::config_path().unwrap();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            format!("backend_url = \"http://{addr}\"\ndevice_token = \"test-token\"\n"),
        )
        .unwrap();

        let discovery = fetch_offline_reachable_folders().await;

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(matches!(discovery, Discovery::Available(grants) if grants.is_empty()));
    }

    #[tokio::test]
    async fn fetch_offline_reachable_folders_makes_no_call_without_a_device_token() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let discovery = fetch_offline_reachable_folders().await;

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(matches!(discovery, Discovery::NotApplicable));
    }

    #[test]
    fn legacy_device_token_advisory_names_the_fix_for_an_access_key_holding_a_device_token() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let config_path = crate::paths::config_path().unwrap();
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "backend_url = \"https://api.example.test\"\ndevice_token = \"legacy-key\"\n",
        )
        .unwrap();

        let advisory = legacy_device_token_advisory(Credential::AccessKey);
        let none_for_account = legacy_device_token_advisory(Credential::Account);

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let advisory = advisory.expect("a stray access key in device_token should be flagged");
        assert!(advisory.contains("valv mount <path> --key <token>"));
        assert!(none_for_account.is_none());
    }

    #[tokio::test]
    async fn status_offline_never_writes_a_config_file_or_installs_the_daemon() {
        let dir = tempfile::tempdir().unwrap();

        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let result = cmd_status(true).await;
        let config_path = dir.path().join(".config/valv/config.toml");
        let config_was_written = config_path.exists();

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(result.is_ok());
        assert!(
            !config_was_written,
            "status must never write a config file: that is ensure_daemon's job, and status must never call it"
        );
    }

    fn mount_status_with(
        name: &str,
        error: Option<&str>,
        update_required: bool,
        pending_ops: u64,
    ) -> MountStatus {
        MountStatus {
            path: "/tmp/valv".into(),
            folder_id: "folder-1".into(),
            name: name.into(),
            scope_node_id: None,
            grant_id: None,
            can_write: true,
            syncing: false,
            pending_ops,
            last_synced_at: None,
            update_required,
            error: error.map(str::to_owned),
            watcher_alive: true,
        }
    }

    fn mount_status_with_watcher(
        error: Option<&str>,
        update_required: bool,
        syncing: bool,
        watcher_alive: bool,
    ) -> MountStatus {
        MountStatus {
            syncing,
            watcher_alive,
            ..mount_status_with("Design", error, update_required, 0)
        }
    }

    #[test]
    fn mount_sync_state_returns_short_tokens_never_a_raw_error_or_full_sentence() {
        assert_eq!(
            mount_sync_state(&mount_status_with("Design", None, false, 0)),
            "synced"
        );
        assert_eq!(
            mount_sync_state(&mount_status_with("Design", None, false, 3)),
            "3 pending"
        );
        assert_eq!(
            mount_sync_state(&mount_status_with("Design", Some("quota exceeded"), false, 0)),
            "error"
        );
        assert_eq!(
            mount_sync_state(&mount_status_with("Design", None, true, 0)),
            "update required"
        );
    }

    #[test]
    fn mount_sync_state_reports_watcher_down_when_nothing_else_applies() {
        assert_eq!(
            mount_sync_state(&mount_status_with_watcher(None, false, false, false)),
            "watcher down"
        );
    }

    #[test]
    fn mount_sync_state_watcher_down_outranks_syncing() {
        assert_eq!(
            mount_sync_state(&mount_status_with_watcher(None, false, true, false)),
            "watcher down"
        );
    }

    #[test]
    fn mount_sync_state_error_outranks_watcher_down() {
        assert_eq!(
            mount_sync_state(&mount_status_with_watcher(
                Some("quota exceeded"),
                false,
                false,
                false
            )),
            "error"
        );
    }

    #[test]
    fn mount_sync_state_update_required_outranks_watcher_down() {
        assert_eq!(
            mount_sync_state(&mount_status_with_watcher(None, true, false, false)),
            "update required"
        );
    }

    #[test]
    fn mount_sync_state_stays_syncing_when_watcher_is_alive() {
        assert_eq!(
            mount_sync_state(&mount_status_with_watcher(None, false, true, true)),
            "syncing"
        );
    }

    #[test]
    fn mount_state_detail_lines_names_the_mount_for_error_and_update_required() {
        let mut status = sample_status();
        status.mounts = vec![
            mount_status_with("Design", Some("quota exceeded"), false, 0),
            mount_status_with("Assets", None, true, 0),
            mount_status_with("Docs", None, false, 0),
        ];

        let lines = mount_state_detail_lines(&status);

        assert_eq!(
            lines,
            vec![
                "Design: quota exceeded".to_owned(),
                "Assets: update required, run `valv update` to fix this.".to_owned(),
            ]
        );
    }

    #[test]
    fn mount_state_cell_and_detail_line_agree_when_a_mount_has_both_flags() {
        let both = mount_status_with("Design", Some("quota exceeded"), true, 0);
        let mut status = sample_status();
        status.mounts = vec![both.clone()];

        assert_eq!(mount_sync_state(&both), "update required");
        assert_eq!(
            mount_state_detail_lines(&status),
            vec!["Design: update required, run `valv update` to fix this.".to_owned()]
        );
    }

    #[test]
    fn build_folder_rows_swaps_state_and_path_for_a_reachable_but_unmounted_folder() {
        let status = sample_status();
        let grants = vec![GrantListEntry {
            grant_id: "g_2".into(),
            folder_id: "folder-2".into(),
            scope_node_id: "root".into(),
            role: Some("collaborator".into()),
            can_read: Some(true),
            can_write: Some(false),
            user_id: Some("user-1".into()),
            device_id: None,
            name: None,
            grantee_email: None,
            device_name: None,
            created_at: None,
            created_by_email: None,
            folder_name: Some("Assets".into()),
        }];

        let rows = build_folder_rows(&status, &Discovery::Available(grants));
        let unmounted_row = rows
            .iter()
            .find(|row| row[0] == "folder-2")
            .expect("the unmounted folder should be listed");

        assert_eq!(unmounted_row[3], "not mounted");
        assert_eq!(unmounted_row[4], "-");
    }

    #[test]
    fn online_and_offline_table_headers_are_human_labels_with_the_id_column_kept() {
        assert_eq!(
            ONLINE_TABLE_HEADERS,
            ["FOLDER ID", "NAME", "ACCESS", "STATE", "PATH"]
        );
        assert_eq!(OFFLINE_TABLE_HEADERS, ["FOLDER ID", "NAME", "PATH"]);
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

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn mount_without_a_source_is_a_usage_error_naming_mount_source_required() {
        let error = parse(&["valv", "mount", "/tmp/data"]).unwrap_err();
        let cli_error = map_clap_error(error);

        assert_eq!(cli_error.payload.code, "mount_source_required");
        assert_eq!(cli_error.exit_code, 2);
    }

    #[test]
    fn mount_with_exactly_one_source_parses() {
        assert!(parse(&["valv", "mount", "/tmp/data", "--new"]).is_ok());
        assert!(parse(&["valv", "mount", "/tmp/data", "--folder", "f_1"]).is_ok());
        assert!(parse(&["valv", "mount", "/tmp/data", "--key", "token"]).is_ok());
    }

    #[test]
    fn mount_with_two_sources_is_a_usage_error() {
        assert!(parse(&["valv", "mount", "/tmp/data", "--new", "--folder", "f_1"]).is_err());
    }

    #[test]
    fn mount_key_accepts_a_token_that_looks_like_a_flag() {
        let cli = parse(&["valv", "mount", "/tmp/data", "--key", "-dGVzdA"]).unwrap();
        let Command::Mount(args) = cli.command else {
            panic!("expected a mount command");
        };
        assert_eq!(args.key.as_deref(), Some("-dGVzdA"));
    }

    #[test]
    fn mount_has_no_read_only_flag() {
        assert!(parse(&["valv", "mount", "/tmp/data", "--new", "--read-only"]).is_err());
    }

    #[test]
    fn share_read_only_without_a_target_is_a_usage_error() {
        let error = parse(&["valv", "share", "/tmp/data", "--read-only"]).unwrap_err();
        let cli_error = map_clap_error(error);

        assert_eq!(cli_error.payload.code, "share_read_only_requires_target");
        assert_eq!(cli_error.exit_code, 2);
    }

    #[test]
    fn share_read_only_with_a_target_parses() {
        assert!(parse(&["valv", "share", "/tmp/data", "--read-only", "--to", "a@b.com"]).is_ok());
        assert!(parse(&["valv", "share", "/tmp/data", "--read-only", "--key", "name"]).is_ok());
    }

    #[test]
    fn bare_share_with_no_flags_still_parses_as_the_list_form() {
        assert!(parse(&["valv", "share", "/tmp/data"]).is_ok());
    }

    #[test]
    fn unmount_and_sync_take_a_path_not_a_folder_flag() {
        assert!(parse(&["valv", "unmount", "/tmp/data"]).is_ok());
        assert!(parse(&["valv", "sync"]).is_ok());
        assert!(parse(&["valv", "sync", "/tmp/data"]).is_ok());
    }

    #[test]
    fn login_replaces_the_auth_namespace() {
        assert!(parse(&["valv", "login"]).is_ok());
        assert!(parse(&["valv", "auth", "login"]).is_err());
    }

    #[test]
    fn grant_and_grants_no_longer_exist() {
        assert!(parse(&["valv", "grant", "create", "/tmp/data", "--to", "a@b.com"]).is_err());
        assert!(parse(&["valv", "grants"]).is_err());
    }

    #[test]
    fn share_bare_and_with_a_target_both_parse() {
        assert!(parse(&["valv", "share", "/tmp/data"]).is_ok());
        assert!(parse(&["valv", "share", "/tmp/data", "--to", "a@b.com"]).is_ok());
        assert!(parse(&["valv", "share", "/tmp/data", "--key", "box"]).is_ok());
    }

    #[test]
    fn share_rejects_both_a_person_and_a_key_at_once() {
        assert!(parse(&[
            "valv",
            "share",
            "/tmp/data",
            "--to",
            "a@b.com",
            "--key",
            "box"
        ])
        .is_err());
    }

    #[test]
    fn unshare_requires_exactly_one_target() {
        assert!(parse(&["valv", "unshare", "/tmp/data"]).is_err());
        assert!(parse(&["valv", "unshare", "/tmp/data", "--to", "a@b.com"]).is_ok());
        assert!(parse(&["valv", "unshare", "/tmp/data", "--id", "g_1"]).is_ok());
    }

    #[tokio::test]
    async fn unshare_json_with_a_handle_is_a_usage_error_before_any_network_call() {
        let error = run_command(
            Command::Unshare(UnshareArgs {
                path: "/tmp/data".into(),
                to: Some("bob@example.com".into()),
                key: None,
                id: None,
                yes: false,
            }),
            true,
        )
        .await
        .unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a handle under --json should be a CliError");

        assert_eq!(cli_error.payload.code, "handle_requires_pinned_id");
        assert_eq!(cli_error.exit_code, 2);
    }

    #[test]
    fn daemon_install_no_longer_exists_restart_does() {
        assert!(parse(&["valv", "daemon", "install"]).is_err());
        assert!(parse(&["valv", "daemon", "restart"]).is_ok());
        assert!(parse(&["valv", "daemon", "uninstall"]).is_ok());
    }

    #[test]
    fn update_has_no_check_flag() {
        assert!(parse(&["valv", "update"]).is_ok());
        assert!(parse(&["valv", "update", "--check"]).is_err());
    }

    #[test]
    fn json_is_a_global_flag_accepted_before_or_after_the_subcommand() {
        assert!(parse(&["valv", "--json", "status"]).is_ok());
        assert!(parse(&["valv", "status", "--json"]).is_ok());
    }

    use crate::daemon::test_support::MockDaemon;

    const ACCESS_KEY_STATUS: &str = r#"{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[],"credential":"access_key","principal":{"type":"access_key","scopes":[]}}"#;
    const ACCOUNT_STATUS: &str = r#"{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[],"credential":"account","principal":{"type":"account","email":"alice@example.com","scopes":[]}}"#;

    fn set_test_home(dir: &std::path::Path) -> Option<std::ffi::OsString> {
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", dir);
        previous
    }

    fn restore_home(previous: Option<std::ffi::OsString>) {
        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[tokio::test]
    async fn pause_under_json_still_pauses_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("POST", "/pause", 204, "")
            .spawn(&socket_path, 1);

        let result = cmd_pause_resume("pause", "Sync paused.", true).await;

        restore_home(previous_home);

        assert!(result.is_ok(), "pause --json should still pause: {result:?}");
    }

    fn write_mount(path: &str, folder_id: &str, mount_token: Option<&str>, can_write: bool) {
        let db_path = crate::paths::data_dir().unwrap().join("sync.db");
        let conn = valv_sync::persistence::open_db(&db_path).unwrap();
        valv_sync::persistence::mounts::upsert_mount(
            &conn,
            path,
            folder_id,
            None,
            None,
            mount_token,
            can_write,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn mount_new_is_refused_locally_on_an_access_key_machine_before_any_mount_request() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("GET", "/status", 200, ACCESS_KEY_STATUS)
            .spawn(&socket_path, 1);

        let result = cmd_mount(
            MountArgs {
                path: "/tmp/data".into(),
                folder: None,
                key: None,
                new: true,
            },
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an access-key refusal should be a CliError");
        assert_eq!(cli_error.payload.code, "access_key_cannot_create_folder");
        assert_eq!(cli_error.exit_code, 77);
    }

    #[tokio::test]
    async fn mount_folder_is_refused_locally_on_an_access_key_machine_before_any_mount_request() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("GET", "/status", 200, ACCESS_KEY_STATUS)
            .spawn(&socket_path, 1);

        let result = cmd_mount(
            MountArgs {
                path: "/tmp/data".into(),
                folder: Some("folder-1".into()),
                key: None,
                new: false,
            },
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an access-key refusal should be a CliError");
        assert_eq!(cli_error.payload.code, "access_key_cannot_mount_folder");
        assert_eq!(cli_error.exit_code, 77);
    }

    fn sync_summary(
        creates: u64,
        versions: u64,
        deletes: u64,
        pulled: i64,
    ) -> SyncSummary {
        SyncSummary {
            creates_submitted: creates,
            versions_submitted: versions,
            deletes_submitted: deletes,
            pulled_ops: pulled,
            errors: 0,
        }
    }

    #[test]
    fn sync_summary_line_collapses_a_no_op_to_one_sentence() {
        assert_eq!(
            sync_summary_line("every mounted folder", &sync_summary(0, 0, 0, 0)),
            "Already up to date."
        );
    }

    #[test]
    fn sync_summary_line_drops_zero_counts_and_avoids_op_log_jargon() {
        assert_eq!(
            sync_summary_line("~/Design", &sync_summary(3, 0, 0, 2)),
            "Synced ~/Design: 3 created, 2 changes received."
        );
    }

    #[test]
    fn version_row_humanizes_created_at_and_size_bytes_for_a_human() {
        let created_at = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        let row = version_row(VersionEntry {
            version_id: "version-1".into(),
            created_at,
            size_bytes: 1_048_576,
            author_device_name: "Alice's MacBook".into(),
            is_conflict_copy: false,
        });

        assert_eq!(row[0], "version-1");
        assert_eq!(row[1], "2 days ago");
        assert_eq!(row[2], "1.0 MB");
        assert_eq!(row[3], "Alice's MacBook");
        assert_eq!(row[4], "no");
    }

    #[test]
    fn version_row_marks_conflict_copies() {
        let row = version_row(VersionEntry {
            version_id: "version-2".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            size_bytes: 42,
            author_device_name: "Device".into(),
            is_conflict_copy: true,
        });

        assert_eq!(row[4], "yes");
    }

    #[test]
    fn conflict_copy_message_names_the_containing_folder_not_the_bare_term() {
        let message = conflict_copy_message("/home/alice/Design/report.md");

        assert_eq!(
            message,
            "Another write happened at the same time, so the restore was saved as a new file in /home/alice/Design instead of overwriting /home/alice/Design/report.md."
        );
        assert!(!message.contains("conflict copy"));
    }

    #[test]
    fn superseded_message_avoids_the_word_superseded() {
        let message = superseded_message("/home/alice/Design/report.md");

        assert_eq!(
            message,
            "/home/alice/Design/report.md was not restored: a newer write already happened, so nothing changed."
        );
        assert!(!message.contains("superseded"));
    }

    #[test]
    fn mount_success_message_leads_with_the_name_never_a_bare_id() {
        assert_eq!(
            mount_success_message(true, "Projects", "/home/alice/Projects"),
            "Created \"Projects\" and mounted it at /home/alice/Projects."
        );
        assert_eq!(
            mount_success_message(false, "Sync Docs", "/home/alice/Documents/Sync"),
            "Attached \"Sync Docs\" at /home/alice/Documents/Sync."
        );
    }

    #[test]
    fn build_mount_request_absolutizes_a_relative_new_path_before_it_reaches_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = fs::canonicalize(dir.path()).unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir_path).unwrap();

        let request = build_mount_request(&MountArgs {
            path: "./Design".into(),
            folder: None,
            key: None,
            new: true,
        });

        std::env::set_current_dir(previous_dir).unwrap();

        let request = request.unwrap();
        assert_eq!(
            request.path,
            dir_path.join("Design").to_string_lossy().into_owned()
        );
        assert!(
            Path::new(&request.path).is_absolute(),
            "a relative --new path must still resolve to an absolute path: {}",
            request.path
        );
    }

    #[test]
    fn build_mount_request_passes_through_an_already_absolute_path_unchanged() {
        let request = build_mount_request(&MountArgs {
            path: "/Users/alice/Design".into(),
            folder: None,
            key: None,
            new: true,
        })
        .unwrap();

        assert_eq!(request.path, "/Users/alice/Design");
    }

    #[tokio::test]
    async fn mount_key_is_never_refused_even_on_a_machine_with_no_credential_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route(
                "POST",
                "/mount",
                200,
                r#"{"folder_id":"folder-1","path":"/tmp/data"}"#,
            )
            .spawn(&socket_path, 1);

        let result = cmd_mount(
            MountArgs {
                path: "/tmp/data".into(),
                folder: None,
                key: Some("a-token".into()),
                new: false,
            },
            true,
        )
        .await;

        restore_home(previous_home);

        assert!(
            result.is_ok(),
            "mount --key must never be refused, even with no prior credential: {result:?}"
        );
    }

    #[tokio::test]
    async fn restore_is_refused_locally_on_an_access_key_machine_when_the_covering_mount_is_read_only(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());

        let mount_dir = dir.path().join("Design");
        fs::create_dir_all(&mount_dir).unwrap();
        let mount_dir = fs::canonicalize(&mount_dir).unwrap();
        let file_path = mount_dir.join("report.md");
        fs::write(&file_path, "content").unwrap();
        write_mount(mount_dir.to_str().unwrap(), "folder-1", None, false);

        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("GET", "/status", 200, ACCESS_KEY_STATUS)
            .spawn(&socket_path, 1);

        let result = cmd_restore(
            file_path.to_str().unwrap().to_owned(),
            "version-1".into(),
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a read-only access key should refuse with a CliError");
        assert_eq!(cli_error.payload.code, "access_key_is_read_only");
        assert_eq!(cli_error.exit_code, 77);
    }

    #[tokio::test]
    async fn restore_is_unaffected_when_the_covering_mount_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());

        let mount_dir = dir.path().join("Design");
        fs::create_dir_all(&mount_dir).unwrap();
        let mount_dir = fs::canonicalize(&mount_dir).unwrap();
        let file_path = mount_dir.join("report.md");
        fs::write(&file_path, "content").unwrap();
        write_mount(mount_dir.to_str().unwrap(), "folder-1", None, true);

        let result = refuse_if_access_key_restore_is_read_only(file_path.to_str().unwrap()).await;

        restore_home(previous_home);

        assert!(result.is_ok(), "a writable mount must never be refused: {result:?}");
    }

    #[tokio::test]
    async fn unmount_of_the_only_key_on_the_machine_requires_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());

        let mount_dir = dir.path().join("Design");
        fs::create_dir_all(&mount_dir).unwrap();
        let mount_dir = fs::canonicalize(&mount_dir).unwrap();
        write_mount(mount_dir.to_str().unwrap(), "folder-1", Some("mount-token"), true);

        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("GET", "/status", 200, ACCESS_KEY_STATUS)
            .spawn(&socket_path, 1);

        let result = cmd_unmount(mount_dir.to_str().unwrap().to_owned(), false, false).await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an unconfirmed destructive unmount should be a CliError");
        assert_eq!(cli_error.payload.code, "confirmation_required");
    }

    #[tokio::test]
    async fn unmount_does_not_warn_when_another_local_mount_still_holds_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());

        let mount_dir = dir.path().join("Design");
        fs::create_dir_all(&mount_dir).unwrap();
        let mount_dir = fs::canonicalize(&mount_dir).unwrap();
        write_mount(mount_dir.to_str().unwrap(), "folder-1", Some("mount-token-1"), true);
        let other_dir = dir.path().join("Assets");
        fs::create_dir_all(&other_dir).unwrap();
        let other_dir = fs::canonicalize(&other_dir).unwrap();
        write_mount(other_dir.to_str().unwrap(), "folder-2", Some("mount-token-2"), true);

        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("GET", "/status", 200, ACCESS_KEY_STATUS)
            .route("DELETE", "/mount", 204, "")
            .spawn(&socket_path, 2);

        let result = cmd_unmount(mount_dir.to_str().unwrap().to_owned(), false, false).await;

        restore_home(previous_home);

        assert!(
            result.is_ok(),
            "unmounting one of two key-holding mounts must not require confirmation: {result:?}"
        );
    }

    #[tokio::test]
    async fn unmount_is_unaffected_on_an_account_machine() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());

        let mount_dir = dir.path().join("Design");
        fs::create_dir_all(&mount_dir).unwrap();
        let mount_dir = fs::canonicalize(&mount_dir).unwrap();
        write_mount(mount_dir.to_str().unwrap(), "folder-1", Some("mount-token"), true);

        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        MockDaemon::new()
            .route("GET", "/status", 200, ACCOUNT_STATUS)
            .route("DELETE", "/mount", 204, "")
            .spawn(&socket_path, 2);

        let result = cmd_unmount(mount_dir.to_str().unwrap().to_owned(), false, false).await;

        restore_home(previous_home);

        assert!(
            result.is_ok(),
            "an account machine's device_token credential must not trigger the warning: {result:?}"
        );
    }

    fn mount_status_json(pending_ops: u64, error: Option<&str>) -> String {
        let error_field = match error {
            Some(error) => format!(r#","error":"{error}""#),
            None => String::new(),
        };
        format!(
            r#"{{"path":"/tmp/x","folder_id":"folder-1","name":"Design","can_write":true,"syncing":false,"pending_ops":{pending_ops},"last_synced_at":null,"update_required":false{error_field}}}"#
        )
    }

    #[tokio::test]
    async fn sync_settles_as_soon_as_pending_ops_is_zero_and_the_pass_has_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(0, None)
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, status_body)
            .spawn(&socket_path, 2);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_secs(2),
            Duration::from_millis(50),
        )
        .await;

        restore_home(previous_home);

        assert!(result.is_ok(), "an already-settled folder should return immediately: {result:?}");
    }

    #[tokio::test]
    async fn sync_times_out_when_pending_ops_never_reaches_zero() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(3, None)
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, status_body)
            .spawn(&socket_path, 200);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_millis(300),
            Duration::from_millis(40),
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a barrier that never settles should time out with a CliError");
        assert_eq!(cli_error.payload.code, "sync_timed_out");
        assert_eq!(cli_error.exit_code, 75);
    }

    #[tokio::test]
    async fn sync_fails_the_barrier_when_a_mount_enters_an_error_state() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(0, Some("quota_exceeded"))
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, status_body)
            .spawn(&socket_path, 2);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_secs(2),
            Duration::from_millis(50),
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a mount error should fail the barrier with a CliError");
        assert_eq!(cli_error.payload.code, "sync_mount_error");
        assert_eq!(cli_error.exit_code, 1);
        assert!(cli_error.payload.message.contains("quota_exceeded"));
    }

    #[tokio::test]
    async fn sync_treats_a_mount_error_during_a_backend_outage_as_retryable_not_a_hard_failure() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let status_body = format!(
            r#"{{"paused":false,"backend_connected":false,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(3, Some("backend_unreachable"))
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, status_body)
            .spawn(&socket_path, 200);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_millis(300),
            Duration::from_millis(40),
        )
        .await;

        restore_home(previous_home);

        let error = result.expect_err(
            "a mount error caused by an unreachable backend must not fail fast: it must wait and time out instead",
        );
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an outage that never clears should time out with a CliError");
        assert_eq!(cli_error.payload.code, "sync_timed_out");
        assert_eq!(cli_error.exit_code, 75);
    }

    #[tokio::test]
    async fn sync_does_not_exit_0_when_a_mount_carries_an_error_during_an_outage_with_no_pending_ops()
    {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let status_body = format!(
            r#"{{"paused":false,"backend_connected":false,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(0, Some("backend_unreachable"))
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, status_body)
            .spawn(&socket_path, 200);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_millis(300),
            Duration::from_millis(40),
        )
        .await;

        restore_home(previous_home);

        let error = result.expect_err(
            "a mount carrying an error during an outage must never count as settled, even with zero pending ops",
        );
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an outage that never clears should time out with a CliError");
        assert_eq!(cli_error.payload.code, "sync_timed_out");
        assert_eq!(cli_error.exit_code, 75);
    }

    #[tokio::test]
    async fn sync_recovers_mid_wait_once_the_backend_reconnects_and_the_mount_error_clears() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let outage_status_body = format!(
            r#"{{"paused":false,"backend_connected":false,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(3, Some("backend_unreachable"))
        );
        let recovered_status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(0, None)
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, outage_status_body)
            .route("GET", "/status", 200, recovered_status_body)
            .spawn(&socket_path, 4);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .await;

        restore_home(previous_home);

        result.expect(
            "a mount that recovers mid-wait once the backend reconnects should settle normally",
        );
    }

    #[tokio::test]
    async fn sync_retries_after_a_transient_pass_error_and_settles_once_a_pass_comes_back_clean() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let dirty_status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(1, None)
        );
        let clean_status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(0, None)
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":1}"#,
            )
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":1,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":0}"#,
            )
            .route("GET", "/status", 200, dirty_status_body)
            .route("GET", "/status", 200, clean_status_body)
            .spawn(&socket_path, 4);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_secs(2),
            Duration::from_millis(20),
        )
        .await;

        restore_home(previous_home);

        let summary = result.expect(
            "a transient per-pass error must not settle immediately, but a later clean pass must",
        );
        assert_eq!(summary.creates_submitted, 1);
    }

    #[tokio::test]
    async fn sync_never_settles_when_every_pass_reports_errors_and_times_out_instead_of_exiting_0()
    {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = set_test_home(dir.path());
        let socket_path = crate::paths::socket_path().unwrap();
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let status_body = format!(
            r#"{{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[{}],"credential":"account"}}"#,
            mount_status_json(1, None)
        );
        MockDaemon::new()
            .route(
                "POST",
                "/sync",
                200,
                r#"{"creates_submitted":0,"versions_submitted":0,"deletes_submitted":0,"pulled_ops":0,"errors":1}"#,
            )
            .route("GET", "/status", 200, status_body)
            .spawn(&socket_path, 200);

        let result = run_sync_barrier(
            None,
            "every mounted folder",
            None,
            Duration::from_millis(300),
            Duration::from_millis(40),
        )
        .await;

        restore_home(previous_home);

        let error = result.expect_err(
            "a pass that never comes back clean must not exit 0: it must retry until the bound, then fail",
        );
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a barrier that never settles should time out with a CliError");
        assert_eq!(cli_error.payload.code, "sync_timed_out");
        assert_eq!(cli_error.exit_code, 75);
    }
}
