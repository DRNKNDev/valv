use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use valv_sync::api_base;

use crate::{
    app::ShareArgs,
    config::{load_backend_url, load_config, CliConfig},
    daemon::probe_credential,
    error::{confirm, CliError, EX_FAILURE, EX_NOPERM},
    format::age_from_now,
    paths::{resolve_mount, resolve_target_path, scope_label},
    table::print_table,
};
use valv_sync::persistence::mounts::LocalMount;
use valv_sync::protocol::ipc::Credential;

async fn backend_response_or_error(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if text.trim().is_empty() {
        return Err(
            CliError::new(EX_FAILURE, "backend_error", format!("backend returned {status}"))
                .into(),
        );
    }
    let (code, message) = parse_backend_error(&text);
    Err(CliError::new(exit_code_for_backend_code(&code, status), stable_backend_code(&code), message).into())
}

fn parse_backend_error(text: &str) -> (String, String) {
    let trimmed = text.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return ("backend_error".into(), trimmed.to_owned());
    };
    let Some(code) = value.get("error").and_then(|error| error.as_str()) else {
        return ("backend_error".into(), trimmed.to_owned());
    };
    let mut parts = vec![code.to_owned()];
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            if key == "error" {
                continue;
            }
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            parts.push(format!("{key}: {value}"));
        }
    }
    (code.to_owned(), parts.join(", "))
}

fn stable_backend_code(code: &str) -> &'static str {
    match code {
        "access_key_cannot_list_grants" => "access_key_cannot_list_grants",
        "access_key_cannot_issue_keys" => "access_key_cannot_issue_keys",
        "access_key_cannot_invite_people" => "access_key_cannot_invite_people",
        "access_key_cannot_revoke" => "access_key_cannot_revoke",
        "access_key_name_taken" => "access_key_name_taken",
        "folder_not_found" => "folder_not_found",
        "grant_not_found" => "grant_not_found",
        "invite_not_found" => "invite_not_found",
        "invite_already_accepted" => "invite_already_accepted",
        "subscription_inactive" => "subscription_inactive",
        "invalid_scope_node_id" => "invalid_scope_node_id",
        "invalid_invited_email" => "invalid_invited_email",
        "insufficient_permission" => "insufficient_permission",
        _ => "backend_error",
    }
}

fn map_backend_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_connect() || error.is_timeout() {
        CliError::backend_unreachable(error.to_string()).into()
    } else {
        error.into()
    }
}

fn exit_code_for_backend_code(code: &str, status: reqwest::StatusCode) -> u8 {
    if code == "access_key_name_taken" {
        return EX_FAILURE;
    }
    if code.starts_with("access_key_") {
        return EX_NOPERM;
    }
    if status.is_server_error() {
        return crate::error::EX_TEMPFAIL;
    }
    EX_FAILURE
}

async fn refuse_if_access_key_cannot_share(args: &ShareArgs) -> Result<()> {
    if !matches!(probe_credential().await, Some(Credential::AccessKey)) {
        return Ok(());
    }
    if args.key.is_some() {
        return Err(CliError::access_key_cannot_issue_keys().into());
    }
    if args.to.is_some() {
        return Err(CliError::access_key_cannot_invite_people().into());
    }
    Ok(())
}

pub(crate) async fn cmd_share_grant(args: ShareArgs, json: bool) -> Result<()> {
    refuse_if_access_key_cannot_share(&args).await?;
    let config = load_config().context("failed to load CLI config for share")?;
    let target = resolve_target_path(&args.path)
        .with_context(|| format!("failed to resolve share scope {}", args.path))?;
    let client = reqwest::Client::new();
    let can_write = !args.read_only;
    if let Some(email) = args.to {
        let scope = scope_label(&target.folder_id, &target.scope_node_id)?;
        let response = client
            .post(format!(
                "{}/folders/{}/invites",
                api_base(&config.backend_url),
                target.folder_id
            ))
            .bearer_auth(config.token()?)
            .json(&InviteCreateRequest {
                invited_email: email.clone(),
                scope_node_id: target.scope_node_id,
                can_write,
            })
            .send()
            .await
            .map_err(map_backend_error)
            .context("failed to send invite creation request")?;
        let response = backend_response_or_error(response).await?;
        let invite = response
            .json::<InviteCreateResponse>()
            .await
            .context("failed to parse invite creation response")?;
        let accept_url = format!(
            "{}/invites/{}/accept",
            api_base(&config.backend_url),
            invite.invite_token
        );
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "invited_email": email,
                    "invite_url": accept_url,
                }))?
            );
        } else {
            let folder_label = resolve_mount(&args.path)
                .ok()
                .and_then(|mount| mount.name)
                .unwrap_or_else(|| args.path.clone());
            println!("{}", invite_receipt_message(&email, &folder_label, &scope, can_write));
            println!("Accept link: {accept_url}");
        }
    } else if let Some(name) = args.key {
        let response = client
            .post(format!(
                "{}/folders/{}/grants",
                api_base(&config.backend_url),
                target.folder_id
            ))
            .bearer_auth(config.token()?)
            .json(&GrantCreateRequest {
                scope_node_id: target.scope_node_id,
                name: name.clone(),
                can_read: true,
                can_write,
            })
            .send()
            .await
            .map_err(map_backend_error)
            .context("failed to send access key creation request")?;
        let response = backend_response_or_error(response).await?;
        let grant = response
            .json::<GrantCreateResponse>()
            .await
            .context("failed to parse access key creation response")?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "grant_id": grant.grant_id,
                    "name": name,
                    "token": grant.token,
                }))?
            );
        } else {
            println!("Created access key {name} ({}).", grant.grant_id);
            println!("One-time token: {}", grant.token);
            println!("Store this token now; it cannot be retrieved again.");
        }
    }
    Ok(())
}

pub(crate) async fn cmd_share_list(path: String, json: bool) -> Result<()> {
    let mount =
        resolve_mount(&path).with_context(|| format!("failed to resolve share path {path}"))?;
    let (grants, invites, show_hint) = fetch_share_listing(&mount).await?;
    print_share_listing(&path, &mount.folder_id, grants, invites, json, show_hint)
}

async fn fetch_share_listing(
    mount: &LocalMount,
) -> Result<(Vec<GrantListEntry>, Vec<InviteEntry>, bool)> {
    let config = share_listing_credential(mount)
        .context("failed to determine a backend credential for share listing")?;
    let client = reqwest::Client::new();

    match fetch_folder_grants(&client, &config, &mount.folder_id).await {
        Ok(grants) => {
            let invites = fetch_folder_invites(&client, &config, &mount.folder_id)
                .await
                .context("failed to fetch pending invites for listing")?;
            Ok((grants, invites, true))
        }
        Err(error) if is_access_key_listing_refusal(&error) => {
            let own_access = fetch_grants(&config)
                .await
                .context("failed to fetch this machine's own access for listing")?
                .into_iter()
                .filter(|grant| grant.folder_id == mount.folder_id)
                .collect::<Vec<_>>();
            Ok((own_access, Vec::new(), false))
        }
        Err(error) => Err(error),
    }
}

fn share_listing_credential(mount: &LocalMount) -> Result<CliConfig> {
    if let Ok(config) = load_config() {
        return Ok(config);
    }
    let backend_url = load_backend_url()?;
    let device_token = mount
        .mount_token
        .clone()
        .ok_or_else(CliError::no_credential)?;
    Ok(CliConfig {
        backend_url,
        device_token: Some(device_token),
    })
}

fn is_access_key_listing_refusal(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<CliError>()
        .is_some_and(|cli_error| cli_error.payload.code == "access_key_cannot_list_grants")
}

#[derive(Debug, Serialize)]
struct ShareListingRow {
    id: String,
    grantee: String,
    scope: String,
    permission: &'static str,
    pending: bool,
}

fn build_listing_rows(
    folder_id: &str,
    grants: Vec<GrantListEntry>,
    invites: Vec<InviteEntry>,
) -> Result<Vec<ShareListingRow>> {
    let mut rows = Vec::with_capacity(grants.len() + invites.len());
    for grant in &grants {
        rows.push(ShareListingRow {
            id: grant_display_id(&grant.grant_id),
            grantee: grant.grantee(),
            scope: scope_label(folder_id, &grant.scope_node_id)?,
            permission: permission_label(grant.can_write.unwrap_or(false)),
            pending: false,
        });
    }
    for invite in &invites {
        rows.push(ShareListingRow {
            id: invite_display_id(&invite.invite_id),
            grantee: invite.invited_email.clone(),
            scope: scope_label(folder_id, &invite.scope_node_id)?,
            permission: permission_label(invite.can_write),
            pending: true,
        });
    }
    Ok(rows)
}

fn share_listing_json(rows: &[ShareListingRow]) -> Result<String> {
    serde_json::to_string(rows).context("failed to serialize share listing as JSON")
}

fn print_share_listing(
    path: &str,
    folder_id: &str,
    grants: Vec<GrantListEntry>,
    invites: Vec<InviteEntry>,
    json: bool,
    show_hint: bool,
) -> Result<()> {
    let rows = build_listing_rows(folder_id, grants, invites)?;
    if json {
        println!("{}", share_listing_json(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("Nobody else can reach {path}.");
    } else {
        let table_rows = rows
            .iter()
            .map(|row| {
                vec![
                    row.id.clone(),
                    row.grantee.clone(),
                    row.scope.clone(),
                    row.permission.to_owned(),
                    if row.pending {
                        "pending".to_owned()
                    } else {
                        "-".to_owned()
                    },
                ]
            })
            .collect::<Vec<_>>();
        print_table(
            &["ID", "GRANTEE", "SCOPE", "PERMISSION", "STATUS"],
            &table_rows,
        );
    }
    if show_hint {
        println!(
            "Add access with: `valv share {path} --to <email>`, or `valv share {path} --key <name>`."
        );
    }
    Ok(())
}

enum PinnedId {
    Grant(String),
    Invite(String),
}

fn parse_pinned_id(id: &str) -> Result<PinnedId> {
    if let Some(rest) = id.strip_prefix("g_") {
        return Ok(PinnedId::Grant(rest.to_owned()));
    }
    if let Some(rest) = id.strip_prefix("i_") {
        return Ok(PinnedId::Invite(rest.to_owned()));
    }
    Err(CliError::usage(
        "invalid_id_prefix",
        format!(
            "{id} is not a valid id: expected a g_ (grant) or i_ (invite) id from `valv share <path>`."
        ),
    )
    .into())
}

enum Selector {
    To(String),
    Key(String),
    Id(PinnedId),
}

impl Selector {
    fn handle(&self) -> String {
        match self {
            Selector::To(email) => email.clone(),
            Selector::Key(name) => name.clone(),
            Selector::Id(PinnedId::Grant(id)) => grant_display_id(id),
            Selector::Id(PinnedId::Invite(id)) => invite_display_id(id),
        }
    }
}

fn build_selector(to: Option<String>, key: Option<String>, id: Option<String>) -> Result<Selector> {
    if let Some(id) = id {
        return Ok(Selector::Id(parse_pinned_id(&id)?));
    }
    if let Some(to) = to {
        return Ok(Selector::To(to));
    }
    if let Some(key) = key {
        return Ok(Selector::Key(key));
    }
    // clap's ArgGroup::required(true) on {to, key, id} guarantees one is Some.
    unreachable!("unshare requires one of --to, --key, or --id")
}

fn matches_grant(grant: &GrantListEntry, selector: &Selector) -> bool {
    match selector {
        Selector::To(email) => grant.grantee_email.as_deref() == Some(email.as_str()),
        Selector::Key(name) => grant.name.as_deref() == Some(name.as_str()),
        Selector::Id(PinnedId::Grant(id)) => grant.grant_id == *id,
        Selector::Id(PinnedId::Invite(_)) => false,
    }
}

fn matches_invite(invite: &InviteEntry, selector: &Selector) -> bool {
    match selector {
        Selector::To(email) => invite.invited_email == *email,
        Selector::Key(_) => false,
        Selector::Id(PinnedId::Invite(id)) => invite.invite_id == *id,
        Selector::Id(PinnedId::Grant(_)) => false,
    }
}

enum RevocableTarget {
    Grant(GrantListEntry),
    Invite(InviteEntry),
}

fn target_handle(target: &RevocableTarget) -> String {
    match target {
        RevocableTarget::Grant(grant) => grant.grantee(),
        RevocableTarget::Invite(invite) => invite.invited_email.clone(),
    }
}

fn target_scope_node_id(target: &RevocableTarget) -> &str {
    match target {
        RevocableTarget::Grant(grant) => &grant.scope_node_id,
        RevocableTarget::Invite(invite) => &invite.scope_node_id,
    }
}

fn target_can_write(target: &RevocableTarget) -> bool {
    match target {
        RevocableTarget::Grant(grant) => grant.can_write.unwrap_or(false),
        RevocableTarget::Invite(invite) => invite.can_write,
    }
}

fn target_created_at(target: &RevocableTarget) -> Option<&str> {
    match target {
        RevocableTarget::Grant(grant) => grant.created_at.as_deref(),
        RevocableTarget::Invite(invite) => Some(invite.created_at.as_str()),
    }
}

fn describe_candidate(folder_id: &str, target: &RevocableTarget) -> Result<String> {
    let scope = scope_label(folder_id, target_scope_node_id(target))?;
    let permission = permission_label(target_can_write(target));
    let id = match target {
        RevocableTarget::Grant(grant) => grant_display_id(&grant.grant_id),
        RevocableTarget::Invite(invite) => invite_display_id(&invite.invite_id),
    };
    Ok(format!(
        "{} ({id}, {scope}, {permission})",
        target_handle(target)
    ))
}

fn confirmation_message(
    target: &RevocableTarget,
    folder_label: &str,
    scope: &str,
    age: Option<&str>,
) -> String {
    let handle = target_handle(target);
    let permission = permission_label(target_can_write(target));
    let age_clause = age.map(|age| format!(", added {age}")).unwrap_or_default();
    match target {
        RevocableTarget::Grant(_) => format!(
            "This will revoke {handle}'s access to {folder_label} ({scope}, {permission}){age_clause}. This cannot be undone."
        ),
        RevocableTarget::Invite(_) => format!(
            "This will cancel {handle}'s pending invite to {folder_label} ({scope}, {permission}){age_clause}."
        ),
    }
}

fn success_message(target: &RevocableTarget, folder_label: &str, scope: &str) -> String {
    let handle = target_handle(target);
    let permission = permission_label(target_can_write(target));
    match target {
        RevocableTarget::Grant(_) => {
            format!("Revoked {handle}'s access to {folder_label} ({scope}, {permission}).")
        }
        RevocableTarget::Invite(_) => format!(
            "Cancelled {handle}'s pending invite to {folder_label} ({scope}, {permission})."
        ),
    }
}

fn permission_label(can_write: bool) -> &'static str {
    if can_write {
        "read/write"
    } else {
        "read-only"
    }
}

fn invite_receipt_message(email: &str, folder_label: &str, scope: &str, can_write: bool) -> String {
    format!(
        "Invited {email} to {folder_label} ({scope}, {}).",
        permission_label(can_write)
    )
}

async fn delete_grant(
    client: &reqwest::Client,
    config: &CliConfig,
    folder_id: &str,
    grant_id: &str,
) -> Result<()> {
    let response = client
        .delete(format!(
            "{}/folders/{}/grants/{}",
            api_base(&config.backend_url),
            folder_id,
            grant_id
        ))
        .bearer_auth(config.token()?)
        .send()
        .await
        .map_err(map_backend_error)
        .context("failed to send revoke request")?;
    backend_response_or_error(response).await?;
    Ok(())
}

async fn cancel_invite(
    client: &reqwest::Client,
    config: &CliConfig,
    folder_id: &str,
    invite_id: &str,
) -> Result<()> {
    let response = client
        .delete(format!(
            "{}/folders/{}/invites/{}",
            api_base(&config.backend_url),
            folder_id,
            invite_id
        ))
        .bearer_auth(config.token()?)
        .send()
        .await
        .map_err(map_backend_error)
        .context("failed to send invite cancellation request")?;
    backend_response_or_error(response).await?;
    Ok(())
}

pub(crate) async fn cmd_unshare(
    path: String,
    to: Option<String>,
    key: Option<String>,
    id: Option<String>,
    yes: bool,
    json: bool,
) -> Result<()> {
    if matches!(probe_credential().await, Some(Credential::AccessKey)) {
        return Err(CliError::access_key_cannot_revoke().into());
    }
    let config = load_config().context("failed to load CLI config for unshare")?;
    let mount =
        resolve_mount(&path).with_context(|| format!("failed to resolve folder for {path}"))?;
    let selector = build_selector(to, key, id)?;
    let client = reqwest::Client::new();

    let grants = fetch_folder_grants(&client, &config, &mount.folder_id)
        .await
        .context("failed to fetch this folder's grants for revocation")?;
    let invites = fetch_folder_invites(&client, &config, &mount.folder_id)
        .await
        .context("failed to fetch this folder's pending invites for revocation")?;

    let mut candidates = grants
        .into_iter()
        .filter(|grant| matches_grant(grant, &selector))
        .map(RevocableTarget::Grant)
        .collect::<Vec<_>>();
    candidates.extend(
        invites
            .into_iter()
            .filter(|invite| matches_invite(invite, &selector))
            .map(RevocableTarget::Invite),
    );

    let target = match candidates.len() {
        0 => return Err(CliError::grant_not_found(selector.handle()).into()),
        1 => candidates.into_iter().next().expect("checked len == 1"),
        _ => {
            let described = candidates
                .iter()
                .map(|candidate| describe_candidate(&mount.folder_id, candidate))
                .collect::<Result<Vec<_>>>()?
                .join("; ");
            return Err(CliError::ambiguous_grant_handle(format!(
                "More than one match: {described}"
            ))
            .into());
        }
    };

    let folder_label = mount.name.clone().unwrap_or_else(|| path.clone());
    let scope = scope_label(&mount.folder_id, target_scope_node_id(&target))?;
    let age = target_created_at(&target).and_then(age_from_now);

    confirm(
        &confirmation_message(&target, &folder_label, &scope, age.as_deref()),
        yes,
    )?;

    match &target {
        RevocableTarget::Grant(grant) => {
            delete_grant(&client, &config, &mount.folder_id, &grant.grant_id).await?;
        }
        RevocableTarget::Invite(invite) => {
            cancel_invite(&client, &config, &mount.folder_id, &invite.invite_id).await?;
        }
    }

    if json {
        println!("{}", unshare_success_json(&target, &mount.folder_id)?);
    } else {
        println!("{}", success_message(&target, &folder_label, &scope));
    }
    Ok(())
}

fn unshare_success_json(target: &RevocableTarget, folder_id: &str) -> Result<String> {
    let id = match target {
        RevocableTarget::Grant(grant) => grant_display_id(&grant.grant_id),
        RevocableTarget::Invite(invite) => invite_display_id(&invite.invite_id),
    };
    serde_json::to_string(&serde_json::json!({
        "id": id,
        "folder_id": folder_id,
        "handle": target_handle(target),
    }))
    .context("failed to serialize unshare result as JSON")
}

fn grant_display_id(grant_id: &str) -> String {
    format!("g_{grant_id}")
}

fn invite_display_id(invite_id: &str) -> String {
    format!("i_{invite_id}")
}

async fn fetch_folder_grants(
    client: &reqwest::Client,
    config: &CliConfig,
    folder_id: &str,
) -> Result<Vec<GrantListEntry>> {
    let response = client
        .get(format!(
            "{}/folders/{}/grants",
            api_base(&config.backend_url),
            folder_id
        ))
        .bearer_auth(config.token()?)
        .send()
        .await
        .map_err(map_backend_error)
        .context("failed to send folder grants list request")?;
    backend_response_or_error(response)
        .await?
        .json::<Vec<GrantListEntry>>()
        .await
        .context("failed to parse folder grants list response")
}

async fn fetch_folder_invites(
    client: &reqwest::Client,
    config: &CliConfig,
    folder_id: &str,
) -> Result<Vec<InviteEntry>> {
    let response = client
        .get(format!(
            "{}/folders/{}/invites",
            api_base(&config.backend_url),
            folder_id
        ))
        .bearer_auth(config.token()?)
        .send()
        .await
        .map_err(map_backend_error)
        .context("failed to send folder invites list request")?;
    backend_response_or_error(response)
        .await?
        .json::<Vec<InviteEntry>>()
        .await
        .context("failed to parse folder invites list response")
}

async fn fetch_grants(config: &CliConfig) -> Result<Vec<GrantListEntry>> {
    fetch_grants_with_client(config, reqwest::Client::new()).await
}

pub(crate) async fn fetch_reachable_grants(
    config: &CliConfig,
    timeout: Duration,
) -> Result<Vec<GrantListEntry>> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build discovery HTTP client")?;
    fetch_grants_with_client(config, client).await
}

async fn fetch_grants_with_client(
    config: &CliConfig,
    client: reqwest::Client,
) -> Result<Vec<GrantListEntry>> {
    let response = client
        .get(format!("{}/grants", api_base(&config.backend_url)))
        .bearer_auth(config.token()?)
        .send()
        .await
        .map_err(map_backend_error)
        .context("failed to send grants list request")?;
    Ok(backend_response_or_error(response)
        .await?
        .json::<Vec<GrantListEntry>>()
        .await
        .context("failed to parse grants list response")?)
}

#[derive(Debug, Serialize)]
struct InviteCreateRequest {
    invited_email: String,
    scope_node_id: String,
    can_write: bool,
}

#[derive(Debug, Deserialize)]
struct InviteCreateResponse {
    invite_token: String,
}

#[derive(Debug, Serialize)]
struct GrantCreateRequest {
    scope_node_id: String,
    name: String,
    can_read: bool,
    can_write: bool,
}

#[derive(Debug, Deserialize)]
struct GrantCreateResponse {
    grant_id: String,
    #[allow(dead_code)]
    device_id: String,
    token: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct GrantListEntry {
    pub(crate) grant_id: String,
    pub(crate) folder_id: String,
    pub(crate) scope_node_id: String,
    pub(crate) role: Option<String>,
    pub(crate) can_read: Option<bool>,
    pub(crate) can_write: Option<bool>,
    pub(crate) user_id: Option<String>,
    pub(crate) device_id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) grantee_email: Option<String>,
    #[serde(default)]
    pub(crate) device_name: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
    #[serde(default)]
    pub(crate) created_by_email: Option<String>,
    #[serde(default)]
    pub(crate) folder_name: Option<String>,
}

impl GrantListEntry {
    fn grantee(&self) -> String {
        self.grantee_email
            .clone()
            .or_else(|| self.name.clone())
            .or_else(|| self.device_name.clone())
            .or_else(|| self.user_id.clone())
            .or_else(|| self.device_id.clone())
            .unwrap_or_else(|| "-".into())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InviteEntry {
    pub(crate) invite_id: String,
    pub(crate) invited_email: String,
    pub(crate) scope_node_id: String,
    pub(crate) can_write: bool,
    pub(crate) created_at: String,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;
    use crate::error::EX_TEMPFAIL;

    fn grant_entry(
        grant_id: &str,
        grantee_email: Option<&str>,
        name: Option<&str>,
        scope_node_id: &str,
        created_at: Option<&str>,
    ) -> GrantListEntry {
        GrantListEntry {
            grant_id: grant_id.into(),
            folder_id: "folder-1".into(),
            scope_node_id: scope_node_id.into(),
            role: Some("collaborator".into()),
            can_read: Some(true),
            can_write: Some(true),
            user_id: None,
            device_id: None,
            name: name.map(str::to_owned),
            grantee_email: grantee_email.map(str::to_owned),
            device_name: None,
            created_at: created_at.map(str::to_owned),
            created_by_email: None,
            folder_name: Some("Design".into()),
        }
    }

    #[test]
    fn grant_list_entry_prefers_grantee_email() {
        let entry = grant_entry("g_1", Some("bob@example.com"), None, "root-1", None);
        assert_eq!(entry.grantee(), "bob@example.com");
    }

    #[test]
    fn matches_grant_resolves_by_email_regardless_of_whose_user_id_it_carries() {
        let grant = grant_entry("g_1", Some("bob@example.com"), None, "root-1", None);
        let selector = Selector::To("bob@example.com".into());
        assert!(matches_grant(&grant, &selector));
        assert!(!matches_grant(
            &grant,
            &Selector::To("someone-else@example.com".into())
        ));
    }

    #[test]
    fn matches_grant_resolves_a_devices_grant_by_key_name() {
        let grant = grant_entry("g_2", None, Some("build-01"), "root-1", None);
        assert!(matches_grant(&grant, &Selector::Key("build-01".into())));
    }

    #[test]
    fn parse_pinned_id_dispatches_on_the_kind_prefix() {
        assert!(matches!(
            parse_pinned_id("g_abc").unwrap(),
            PinnedId::Grant(id) if id == "abc"
        ));
        assert!(matches!(
            parse_pinned_id("i_abc").unwrap(),
            PinnedId::Invite(id) if id == "abc"
        ));
        assert!(parse_pinned_id("abc").is_err());
    }

    #[test]
    fn invite_create_request_serializes_can_write() {
        let writable = serde_json::to_value(InviteCreateRequest {
            invited_email: "friend@example.com".into(),
            scope_node_id: "node-1".into(),
            can_write: true,
        })
        .unwrap();
        let read_only = serde_json::to_value(InviteCreateRequest {
            invited_email: "friend@example.com".into(),
            scope_node_id: "node-1".into(),
            can_write: false,
        })
        .unwrap();

        assert_eq!(writable["can_write"], true);
        assert_eq!(read_only["can_write"], false);
    }

    #[test]
    fn share_listing_json_emits_array_without_human_table_text() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let rows = build_listing_rows(
            "folder-1",
            vec![grant_entry(
                "grant-1",
                Some("bob@example.com"),
                None,
                "missing-node",
                None,
            )],
            Vec::new(),
        )
        .unwrap();

        restore_home(previous_home);

        let output = share_listing_json(&rows).unwrap();

        assert!(output.starts_with('['));
        assert!(output.contains("g_grant-1"));
        assert!(!output.contains("grantee scope permission"));
    }

    #[test]
    fn confirmation_message_names_the_grants_age() {
        let created_at = (Utc::now() - chrono::Duration::minutes(4)).to_rfc3339();
        let grant = grant_entry(
            "grant-1",
            None,
            Some("build-01"),
            "root-1",
            Some(&created_at),
        );
        let target = RevocableTarget::Grant(grant);
        let age = target_created_at(&target).and_then(age_from_now);

        let message = confirmation_message(&target, "Design", "Entire Folder", age.as_deref());

        assert!(message.contains("build-01"));
        assert!(message.contains("Design"));
        assert!(message.contains("Entire Folder"));
        assert!(message.contains("added 4 minutes ago"));
    }

    #[test]
    fn unshare_success_json_emits_a_single_object_without_human_prose() {
        let grant = grant_entry("g_9f31bd", Some("bob@example.com"), None, "root-1", None);
        let target = RevocableTarget::Grant(grant);

        let output = unshare_success_json(&target, "folder-1").unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(value["id"], "g_g_9f31bd");
        assert_eq!(value["folder_id"], "folder-1");
        assert_eq!(value["handle"], "bob@example.com");
        assert!(!output.contains("Revoked"));
    }

    #[test]
    fn invite_receipt_message_names_the_grantee_folder_scope_and_permission() {
        let message = invite_receipt_message("bob@example.com", "Design", "Entire Folder", true);

        assert_eq!(
            message,
            "Invited bob@example.com to Design (Entire Folder, read/write)."
        );
    }

    #[test]
    fn invite_receipt_message_names_read_only_permission() {
        let message = invite_receipt_message("bob@example.com", "Design", "Entire Folder", false);

        assert_eq!(
            message,
            "Invited bob@example.com to Design (Entire Folder, read-only)."
        );
    }

    #[test]
    fn success_message_names_the_handle_and_scope_never_a_bare_id() {
        let grant = grant_entry("g_9f31bd", Some("bob@example.com"), None, "root-1", None);
        let target = RevocableTarget::Grant(grant);

        let message = success_message(&target, "Design", "Entire Folder");

        assert_eq!(
            message,
            "Revoked bob@example.com's access to Design (Entire Folder, read/write)."
        );
        assert!(!message.contains("g_9f31bd"));
    }

    #[test]
    fn exit_code_for_backend_code_maps_access_key_refusals_to_77_and_name_taken_to_1() {
        assert_eq!(
            exit_code_for_backend_code("access_key_cannot_revoke", reqwest::StatusCode::FORBIDDEN),
            EX_NOPERM
        );
        assert_eq!(
            exit_code_for_backend_code("access_key_name_taken", reqwest::StatusCode::CONFLICT),
            EX_FAILURE
        );
    }

    #[tokio::test]
    async fn structured_backend_error_body_renders_readably() {
        let _guard = crate::LOOPBACK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind loopback test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer).await.unwrap();
            let body = r#"{"error":"subscription_inactive","status":"none"}"#;
            let response = format!(
                "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let response = reqwest::get(format!("http://{addr}/fail")).await.unwrap();
        let error = backend_response_or_error(response).await.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a structured backend error should be a CliError");

        assert!(cli_error.payload.message.contains("subscription_inactive"));
        assert!(cli_error.payload.message.contains("none"));
        assert!(!cli_error.payload.message.contains("HTTP status client error"));
        assert_eq!(cli_error.exit_code, EX_FAILURE);
    }

    #[tokio::test]
    async fn map_backend_error_classifies_a_connection_refusal_as_backend_unreachable() {
        let _guard = crate::LOOPBACK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind loopback test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let error = reqwest::get(format!("http://{addr}")).await.unwrap_err();
        let mapped = map_backend_error(error);
        let cli_error = mapped
            .downcast_ref::<CliError>()
            .expect("a connection refusal should map to a CliError");

        assert_eq!(cli_error.payload.code, "backend_unreachable");
        assert_eq!(cli_error.exit_code, EX_TEMPFAIL);
    }

    struct MockBackend {
        routes: HashMap<(&'static str, String), (u16, String)>,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                routes: HashMap::new(),
            }
        }

        fn route(mut self, method: &'static str, path: impl Into<String>, status: u16, body: impl Into<String>) -> Self {
            self.routes.insert((method, path.into()), (status, body.into()));
            self
        }

        async fn serve(listener: TcpListener, routes: HashMap<(&'static str, String), (u16, String)>, request_count: usize) {
            for _ in 0..request_count {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 8192];
                let n = match stream.read(&mut buffer).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
                let request_line = request.lines().next().unwrap_or_default();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default();
                let path = parts.next().unwrap_or_default().to_owned();
                let (status, body) = routes
                    .get(&(method, path))
                    .cloned()
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
        }

        async fn spawn(self, request_count: usize) -> Result<String> {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let addr = listener.local_addr()?;
            tokio::spawn(Self::serve(listener, self.routes, request_count));
            Ok(format!("http://{addr}"))
        }
    }

    // Callers must hold `crate::HOME_ENV_LOCK` for the whole test body: HOME
    // is process-global and read again well after this function returns.
    fn write_test_home(home: &std::path::Path, backend_url: &str, folder_id: &str, mount_path: &str, folder_name: &str, root_node_id: &str) {
        std::env::set_var("HOME", home);

        let config_path = crate::paths::config_path().unwrap();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            format!("backend_url = \"{backend_url}\"\ndevice_token = \"test-token\"\n"),
        )
        .unwrap();

        let db_path = crate::paths::data_dir().unwrap().join("sync.db");
        let conn = valv_sync::persistence::open_db(&db_path).unwrap();
        valv_sync::persistence::mounts::upsert_mount(
            &conn, mount_path, folder_id, None, None, None, true,
        )
        .unwrap();
        valv_sync::persistence::mounts::set_mount_name(&conn, mount_path, folder_name).unwrap();
        valv_sync::persistence::nodes::upsert_node(
            &conn,
            &valv_sync::persistence::LocalNode {
                node_id: root_node_id.into(),
                folder_id: folder_id.into(),
                parent_id: None,
                name: String::new(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 0,
                deleted_at: None,
            },
        )
        .unwrap();
    }

    fn restore_home(previous: Option<std::ffi::OsString>) {
        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[tokio::test]
    async fn share_list_exits_75_backend_unreachable_when_the_backend_is_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        let _loopback_guard = crate::LOOPBACK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind loopback test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        drop(listener);

        write_test_home(
            dir.path(),
            &format!("http://{addr}"),
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );

        let result = cmd_share_list("/home/test/Design".into(), false).await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an unreachable backend should be a CliError");
        assert_eq!(cli_error.payload.code, "backend_unreachable");
        assert_eq!(cli_error.exit_code, EX_TEMPFAIL);
    }

    #[tokio::test]
    async fn unshare_revokes_a_collaborators_grant_the_case_dead_on_main() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                200,
                r#"[{"grant_id":"grant-1","folder_id":"folder-1","scope_node_id":"root-1","role":"collaborator","can_read":true,"can_write":true,"user_id":"bob-user-id","device_id":null,"name":null,"grantee_email":"bob@example.com","device_name":null,"created_at":"2026-07-13T12:00:00Z","created_by_email":"owner@example.com"}]"#,
            )
            .route("GET", "/api/folders/folder-1/invites", 200, "[]")
            .route("DELETE", "/api/folders/folder-1/grants/grant-1", 204, "")
            .route("GET", "/api/grants", 200, "[]");
        let backend_url = backend.spawn(4).await.unwrap();

        write_test_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );

        let result = cmd_unshare(
            "/home/test/Design".into(),
            Some("bob@example.com".into()),
            None,
            None,
            true,
            false,
        )
        .await;

        restore_home(previous_home);

        assert!(result.is_ok(), "unshare should succeed: {result:?}");
    }

    #[tokio::test]
    async fn unshare_json_succeeds_and_still_revokes_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                200,
                r#"[{"grant_id":"grant-1","folder_id":"folder-1","scope_node_id":"root-1","role":"collaborator","can_read":true,"can_write":true,"user_id":"bob-user-id","device_id":null,"name":null,"grantee_email":"bob@example.com","device_name":null,"created_at":"2026-07-13T12:00:00Z","created_by_email":"owner@example.com"}]"#,
            )
            .route("GET", "/api/folders/folder-1/invites", 200, "[]")
            .route("DELETE", "/api/folders/folder-1/grants/grant-1", 204, "");
        let backend_url = backend.spawn(3).await.unwrap();

        write_test_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );

        let result = cmd_unshare(
            "/home/test/Design".into(),
            None,
            None,
            Some("g_grant-1".into()),
            true,
            true,
        )
        .await;

        restore_home(previous_home);

        assert!(result.is_ok(), "unshare --json should still revoke the grant: {result:?}");
    }

    #[tokio::test]
    async fn unshare_revokes_a_keys_grant() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                200,
                r#"[{"grant_id":"grant-2","folder_id":"folder-1","scope_node_id":"root-1","role":"collaborator","can_read":true,"can_write":false,"user_id":null,"device_id":"device-1","name":"build-01","grantee_email":null,"device_name":"Build Box","created_at":"2026-07-13T12:00:00Z","created_by_email":"owner@example.com"}]"#,
            )
            .route("GET", "/api/folders/folder-1/invites", 200, "[]")
            .route("DELETE", "/api/folders/folder-1/grants/grant-2", 204, "")
            .route("GET", "/api/grants", 200, "[]");
        let backend_url = backend.spawn(4).await.unwrap();

        write_test_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );

        let result = cmd_unshare(
            "/home/test/Design".into(),
            None,
            Some("build-01".into()),
            None,
            true,
            false,
        )
        .await;

        restore_home(previous_home);

        assert!(result.is_ok(), "unshare should succeed: {result:?}");
    }

    #[tokio::test]
    async fn unshare_refuses_and_lists_an_ambiguous_handle() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                200,
                r#"[
                    {"grant_id":"grant-1","folder_id":"folder-1","scope_node_id":"root-1","role":"collaborator","can_read":true,"can_write":true,"user_id":"bob-user-id","device_id":null,"name":null,"grantee_email":"bob@example.com","device_name":null,"created_at":"2026-07-13T12:00:00Z","created_by_email":null},
                    {"grant_id":"grant-2","folder_id":"folder-1","scope_node_id":"sub-1","role":"collaborator","can_read":true,"can_write":false,"user_id":"bob-user-id","device_id":null,"name":null,"grantee_email":"bob@example.com","device_name":null,"created_at":"2026-07-13T12:00:00Z","created_by_email":null}
                ]"#,
            )
            .route("GET", "/api/folders/folder-1/invites", 200, "[]");
        let backend_url = backend.spawn(2).await.unwrap();

        write_test_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );
        {
            let conn = valv_sync::persistence::open_db(
                &crate::paths::data_dir().unwrap().join("sync.db"),
            )
            .unwrap();
            valv_sync::persistence::nodes::upsert_node(
                &conn,
                &valv_sync::persistence::LocalNode {
                    node_id: "sub-1".into(),
                    folder_id: "folder-1".into(),
                    parent_id: Some("root-1".into()),
                    name: "assets".into(),
                    node_type: "folder".into(),
                    current_version_id: None,
                    server_seq: 1,
                    deleted_at: None,
                },
            )
            .unwrap();
        }

        let result = cmd_unshare(
            "/home/test/Design".into(),
            Some("bob@example.com".into()),
            None,
            None,
            true,
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("ambiguity should be a CliError");
        assert_eq!(cli_error.payload.code, "ambiguous_grant_handle");
        assert_eq!(cli_error.exit_code, EX_FAILURE);
        assert!(cli_error.payload.message.contains("g_grant-1"));
        assert!(cli_error.payload.message.contains("g_grant-2"));
        assert!(cli_error.payload.message.contains("Entire Folder"));
        assert!(cli_error.payload.message.contains("assets"));
    }

    #[tokio::test]
    async fn unshare_with_no_tty_and_no_yes_refuses_rather_than_prompting() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                200,
                r#"[{"grant_id":"grant-1","folder_id":"folder-1","scope_node_id":"root-1","role":"collaborator","can_read":true,"can_write":true,"user_id":"bob-user-id","device_id":null,"name":null,"grantee_email":"bob@example.com","device_name":null,"created_at":"2026-07-13T12:00:00Z","created_by_email":null}]"#,
            )
            .route("GET", "/api/folders/folder-1/invites", 200, "[]");
        let backend_url = backend.spawn(2).await.unwrap();

        write_test_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );

        let result = cmd_unshare(
            "/home/test/Design".into(),
            Some("bob@example.com".into()),
            None,
            None,
            false,
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("a refused confirmation should be a CliError");
        assert_eq!(cli_error.payload.code, "confirmation_required");
        assert_eq!(cli_error.exit_code, EX_FAILURE);
    }

    #[tokio::test]
    async fn unshare_cancels_a_pending_invite_by_email() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route("GET", "/api/folders/folder-1/grants", 200, "[]")
            .route(
                "GET",
                "/api/folders/folder-1/invites",
                200,
                r#"[{"invite_id":"invite-1","invited_email":"carol@example.com","scope_node_id":"root-1","can_write":true,"created_at":"2026-07-13T12:00:00Z"}]"#,
            )
            .route("DELETE", "/api/folders/folder-1/invites/invite-1", 204, "");
        let backend_url = backend.spawn(3).await.unwrap();

        write_test_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
        );

        let result = cmd_unshare(
            "/home/test/Design".into(),
            Some("carol@example.com".into()),
            None,
            None,
            true,
            false,
        )
        .await;

        restore_home(previous_home);

        assert!(result.is_ok(), "cancelling the invite should succeed: {result:?}");
    }

    const ACCESS_KEY_STATUS: &str = r#"{"paused":false,"backend_connected":true,"version":"0.1.0","update_required":false,"mounts":[],"credential":"access_key","principal":{"type":"access_key","scopes":[]}}"#;

    fn spawn_access_key_daemon() {
        let socket_path = crate::paths::socket_path().unwrap();
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        crate::daemon::test_support::MockDaemon::new()
            .route("GET", "/status", 200, ACCESS_KEY_STATUS)
            .spawn(&socket_path, 1);
    }

    #[tokio::test]
    async fn share_key_is_refused_locally_on_an_access_key_machine_before_any_backend_call() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        spawn_access_key_daemon();

        let result = cmd_share_grant(
            ShareArgs {
                path: "/tmp/data".into(),
                to: None,
                key: Some("second-box".into()),
                read_only: false,
            },
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an access-key refusal should be a CliError");
        assert_eq!(cli_error.payload.code, "access_key_cannot_issue_keys");
        assert_eq!(cli_error.exit_code, EX_NOPERM);
    }

    #[tokio::test]
    async fn share_to_is_refused_locally_on_an_access_key_machine_before_any_backend_call() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        spawn_access_key_daemon();

        let result = cmd_share_grant(
            ShareArgs {
                path: "/tmp/data".into(),
                to: Some("bob@example.com".into()),
                key: None,
                read_only: false,
            },
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an access-key refusal should be a CliError");
        assert_eq!(cli_error.payload.code, "access_key_cannot_invite_people");
        assert_eq!(cli_error.exit_code, EX_NOPERM);
    }

    #[tokio::test]
    async fn unshare_is_refused_locally_on_an_access_key_machine_for_any_target_including_its_own(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        spawn_access_key_daemon();

        let result = cmd_unshare(
            "/tmp/data".into(),
            None,
            Some("its-own-name".into()),
            None,
            true,
            false,
        )
        .await;

        restore_home(previous_home);

        let error = result.unwrap_err();
        let cli_error = error
            .downcast_ref::<CliError>()
            .expect("an access-key refusal should be a CliError");
        assert_eq!(cli_error.payload.code, "access_key_cannot_revoke");
        assert_eq!(cli_error.exit_code, EX_NOPERM);
    }

    fn write_access_key_home(
        home: &std::path::Path,
        backend_url: &str,
        folder_id: &str,
        mount_path: &str,
        folder_name: &str,
        root_node_id: &str,
        mount_token: &str,
    ) {
        std::env::set_var("HOME", home);

        let config_path = crate::paths::config_path().unwrap();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            format!("backend_url = \"{backend_url}\"\ndevice_name = \"Access Key Box\"\n"),
        )
        .unwrap();

        let db_path = crate::paths::data_dir().unwrap().join("sync.db");
        let conn = valv_sync::persistence::open_db(&db_path).unwrap();
        valv_sync::persistence::mounts::upsert_mount(
            &conn,
            mount_path,
            folder_id,
            None,
            None,
            Some(mount_token),
            true,
        )
        .unwrap();
        valv_sync::persistence::mounts::set_mount_name(&conn, mount_path, folder_name).unwrap();
        valv_sync::persistence::nodes::upsert_node(
            &conn,
            &valv_sync::persistence::LocalNode {
                node_id: root_node_id.into(),
                folder_id: folder_id.into(),
                parent_id: None,
                name: String::new(),
                node_type: "folder".into(),
                current_version_id: None,
                server_seq: 0,
                deleted_at: None,
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn share_list_works_on_an_access_key_only_machine_with_no_device_token() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                403,
                r#"{"error":"access_key_cannot_list_grants"}"#,
            )
            .route(
                "GET",
                "/api/grants",
                200,
                r#"[{"grant_id":"grant-1","folder_id":"folder-1","scope_node_id":"root-1","role":null,"can_read":true,"can_write":true,"user_id":null,"device_id":"device-1","name":"build-01","grantee_email":null,"device_name":"Build Box","created_at":null,"created_by_email":null,"folder_name":"Design"}]"#,
            );
        let backend_url = backend.spawn(2).await.unwrap();

        write_access_key_home(
            dir.path(),
            &backend_url,
            "folder-1",
            "/home/test/Design",
            "Design",
            "root-1",
            "mount-token-xyz",
        );
        let config_text =
            std::fs::read_to_string(crate::paths::config_path().unwrap()).unwrap();
        assert!(!config_text.contains("device_token"));

        let result = cmd_share_list("/home/test/Design".into(), false).await;

        restore_home(previous_home);

        assert!(
            result.is_ok(),
            "share listing should succeed with only a mount token: {result:?}"
        );
    }

    #[tokio::test]
    async fn share_list_on_an_access_key_only_machine_never_names_another_principal() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let config_path = crate::paths::config_path().unwrap();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

        let backend = MockBackend::new()
            .route(
                "GET",
                "/api/folders/folder-1/grants",
                403,
                r#"{"error":"access_key_cannot_list_grants"}"#,
            )
            .route(
                "GET",
                "/api/grants",
                200,
                r#"[
                    {"grant_id":"grant-1","folder_id":"folder-1","scope_node_id":"root-1","role":null,"can_read":true,"can_write":true,"user_id":null,"device_id":"device-1","name":"build-01","grantee_email":null,"device_name":"Build Box","created_at":null,"created_by_email":null,"folder_name":"Design"},
                    {"grant_id":"grant-2","folder_id":"folder-2","scope_node_id":"root-2","role":null,"can_read":true,"can_write":true,"user_id":null,"device_id":"device-2","name":null,"grantee_email":"someone-else@example.com","device_name":null,"created_at":null,"created_by_email":null,"folder_name":"Other"}
                ]"#,
            );
        let backend_url = backend.spawn(2).await.unwrap();
        std::fs::write(
            &config_path,
            format!("backend_url = \"{backend_url}\"\ndevice_name = \"Access Key Box\"\n"),
        )
        .unwrap();

        let mount = valv_sync::persistence::mounts::LocalMount {
            path: "/home/test/Design".into(),
            folder_id: "folder-1".into(),
            grant_id: None,
            scope_node_id: None,
            mount_token: Some("mount-token-xyz".into()),
            cursor: 0,
            can_write: true,
            name: Some("Design".into()),
        };

        let result = fetch_share_listing(&mount).await;

        restore_home(previous_home);

        let (grants, invites, show_hint) = result.unwrap();
        assert!(invites.is_empty());
        assert!(!show_hint);
        assert_eq!(grants.len(), 1, "the other folder's grant must be filtered out");
        assert!(
            grants.iter().all(|grant| grant.grantee_email.is_none()),
            "an access key's own listing must never carry another principal's email"
        );
    }
}
