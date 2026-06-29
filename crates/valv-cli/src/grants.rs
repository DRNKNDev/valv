use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use valv_sync::api_base;

use crate::{
    app::GrantCreateArgs,
    config::{load_config, CliConfig},
    paths::{first_mount_folder_id, resolve_target_path},
};

pub(crate) async fn cmd_grant_create(args: GrantCreateArgs) -> Result<()> {
    let config = load_config()?;
    let target = resolve_target_path(&args.node_path)?;
    let client = reqwest::Client::new();
    let can_write = !args.read_only;
    if let Some(email) = args.to {
        let response = client
            .post(format!(
                "{}/folders/{}/invites",
                api_base(&config.backend_url),
                target.folder_id
            ))
            .bearer_auth(&config.device_token)
            .json(&InviteCreateRequest {
                invited_email: email,
                scope_node_id: target.scope_node_id,
            })
            .send()
            .await?
            .error_for_status()?;
        let invite = response.json::<InviteCreateResponse>().await?;
        println!(
            "Invite URL: {}/invites/{}/accept",
            api_base(&config.backend_url),
            invite.invite_token
        );
    } else if let Some(name) = args.device {
        let response = client
            .post(format!(
                "{}/folders/{}/grants",
                api_base(&config.backend_url),
                target.folder_id
            ))
            .bearer_auth(&config.device_token)
            .json(&GrantCreateRequest {
                scope_node_id: target.scope_node_id,
                name,
                can_read: true,
                can_write,
            })
            .send()
            .await?
            .error_for_status()?;
        let grant = response.json::<GrantCreateResponse>().await?;
        println!("Device token: {}", grant.token);
        println!("Grant ID: {}", grant.grant_id);
        println!("Device ID: {}", grant.device_id);
        println!("Store this token now; it cannot be retrieved again.");
    }
    Ok(())
}

pub(crate) async fn cmd_grants(folder_path: Option<String>) -> Result<()> {
    let config = load_config()?;
    let folder_id = match folder_path {
        Some(path) => resolve_target_path(&path)?.folder_id,
        None => first_mount_folder_id()?,
    };
    let grants = fetch_grants(&config).await?;
    println!("grant_id\tscope\tgrantee\trole\tcan_read\tcan_write");
    for grant in grants
        .into_iter()
        .filter(|grant| grant.folder_id == folder_id)
    {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            grant.grant_id,
            grant.scope_node_id,
            grant.grantee(),
            grant.role.unwrap_or_else(|| "-".into()),
            grant.can_read.unwrap_or(false),
            grant.can_write.unwrap_or(false)
        );
    }
    Ok(())
}

pub(crate) async fn cmd_grant_revoke(grant_id: String) -> Result<()> {
    let config = load_config()?;
    let grants = fetch_grants(&config).await?;
    let folder_id = grants
        .iter()
        .find(|grant| grant.grant_id == grant_id)
        .map(|grant| grant.folder_id.clone())
        .ok_or_else(|| anyhow!("grant not found: {grant_id}"))?;
    reqwest::Client::new()
        .delete(format!(
            "{}/folders/{}/grants/{}",
            api_base(&config.backend_url),
            folder_id,
            grant_id
        ))
        .bearer_auth(&config.device_token)
        .send()
        .await?
        .error_for_status()?;
    println!("Grant {grant_id} revoked");
    Ok(())
}

async fn fetch_grants(config: &CliConfig) -> Result<Vec<GrantListEntry>> {
    Ok(reqwest::Client::new()
        .get(format!("{}/grants", api_base(&config.backend_url)))
        .bearer_auth(&config.device_token)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<GrantListEntry>>()
        .await?)
}

#[derive(Debug, Serialize)]
struct InviteCreateRequest {
    invited_email: String,
    scope_node_id: String,
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
    device_id: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct GrantListEntry {
    grant_id: String,
    folder_id: String,
    scope_node_id: String,
    role: Option<String>,
    can_read: Option<bool>,
    can_write: Option<bool>,
    user_id: Option<String>,
    device_id: Option<String>,
}

impl GrantListEntry {
    fn grantee(&self) -> String {
        self.user_id
            .clone()
            .or_else(|| self.device_id.clone())
            .unwrap_or_else(|| "-".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_list_entry_prefers_user_grantee() {
        let entry = GrantListEntry {
            grant_id: "g".into(),
            folder_id: "f".into(),
            scope_node_id: "n".into(),
            role: None,
            can_read: None,
            can_write: None,
            user_id: Some("u".into()),
            device_id: Some("d".into()),
        };

        assert_eq!(entry.grantee(), "u");
    }
}
