use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use valv_sync::api_base;

use crate::{
    app::GrantCreateArgs,
    config::{load_config, CliConfig},
    paths::{first_mount_folder_id, resolve_target_path},
    table::print_table,
};

async fn backend_response_or_error(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let message = readable_backend_error(&text);
    if message.trim().is_empty() {
        Err(anyhow!("backend returned {status}"))
    } else {
        Err(anyhow!("{message}"))
    }
}

fn readable_backend_error(text: &str) -> String {
    let trimmed = text.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return trimmed.to_owned();
    };
    let Some(error) = value.get("error").and_then(|error| error.as_str()) else {
        return trimmed.to_owned();
    };
    let mut parts = vec![error.to_owned()];
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
    parts.join(", ")
}

pub(crate) async fn cmd_grant_create(args: GrantCreateArgs) -> Result<()> {
    let config = load_config().context("failed to load CLI config for grant creation")?;
    let target = resolve_target_path(&args.node_path)
        .with_context(|| format!("failed to resolve grant scope {}", args.node_path))?;
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
                can_write,
            })
            .send()
            .await
            .context("failed to send invite creation request")?;
        let response = backend_response_or_error(response).await?;
        let invite = response
            .json::<InviteCreateResponse>()
            .await
            .context("failed to parse invite creation response")?;
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
            .await
            .context("failed to send device grant creation request")?;
        let response = backend_response_or_error(response).await?;
        let grant = response
            .json::<GrantCreateResponse>()
            .await
            .context("failed to parse device grant creation response")?;
        println!(
            "Created device grant {}: device {} can mount this scope",
            grant.grant_id, grant.device_id
        );
        println!("One-time token: {}", grant.token);
        println!("Store this token now; it cannot be retrieved again.");
    }
    Ok(())
}

pub(crate) async fn cmd_grants(folder_path: Option<String>, json: bool) -> Result<()> {
    let config = load_config().context("failed to load CLI config for grant listing")?;
    let folder_id = match folder_path {
        Some(path) => {
            resolve_target_path(&path)
                .with_context(|| format!("failed to resolve grants path {path}"))?
                .folder_id
        }
        None => first_mount_folder_id().context("failed to choose mounted folder for grants")?,
    };
    let grants = fetch_grants(&config)
        .await
        .context("failed to fetch grants for listing")?;
    let grants = grants
        .into_iter()
        .filter(|grant| grant.folder_id == folder_id)
        .collect::<Vec<_>>();
    if json {
        println!("{}", grants_json(&grants)?);
        return Ok(());
    }
    let rows = grants
        .into_iter()
        .map(|grant| {
            let grantee = grant.grantee();
            vec![
                grant.grant_id,
                grant.scope_node_id,
                grantee,
                grant.role.unwrap_or_else(|| "-".into()),
                grant.can_read.unwrap_or(false).to_string(),
                grant.can_write.unwrap_or(false).to_string(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "grant_id",
            "scope",
            "grantee",
            "role",
            "can_read",
            "can_write",
        ],
        &rows,
    );
    Ok(())
}

fn grants_json(grants: &[GrantListEntry]) -> Result<String> {
    serde_json::to_string(grants).context("failed to serialize grants as JSON")
}

pub(crate) async fn cmd_grant_revoke(grant_id: String) -> Result<()> {
    let config = load_config().context("failed to load CLI config for grant revocation")?;
    let grants = fetch_grants(&config)
        .await
        .context("failed to fetch grants for revocation")?;
    let folder_id = grants
        .iter()
        .find(|grant| grant.grant_id == grant_id)
        .map(|grant| grant.folder_id.clone())
        .ok_or_else(|| anyhow!("grant not found: {grant_id}"))?;
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/folders/{}/grants/{}",
            api_base(&config.backend_url),
            folder_id,
            grant_id
        ))
        .bearer_auth(&config.device_token)
        .send()
        .await
        .with_context(|| format!("failed to send revoke request for grant {grant_id}"))?;
    backend_response_or_error(response).await?;
    println!("Revoked grant {grant_id}: access removed");
    Ok(())
}

async fn fetch_grants(config: &CliConfig) -> Result<Vec<GrantListEntry>> {
    let response = reqwest::Client::new()
        .get(format!("{}/grants", api_base(&config.backend_url)))
        .bearer_auth(&config.device_token)
        .send()
        .await
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
    device_id: String,
    token: String,
}

#[derive(Debug, Deserialize, Serialize)]
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
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

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
    fn grants_json_emits_array_without_human_table_text() {
        let grants = vec![GrantListEntry {
            grant_id: "grant-1".into(),
            folder_id: "folder-1".into(),
            scope_node_id: "node-1".into(),
            role: Some("owner".into()),
            can_read: Some(true),
            can_write: Some(true),
            user_id: Some("user-1".into()),
            device_id: None,
        }];

        let output = grants_json(&grants).unwrap();
        let parsed: Vec<GrantListEntry> = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed[0].grant_id, "grant-1");
        assert!(output.starts_with('['));
        assert!(!output.contains("grant_id scope grantee"));
    }

    #[tokio::test]
    async fn structured_backend_error_body_renders_readably() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
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
        let message = backend_response_or_error(response)
            .await
            .unwrap_err()
            .to_string();

        assert!(message.contains("subscription_inactive"));
        assert!(message.contains("none"));
        assert!(!message.contains("HTTP status client error"));
    }
}
