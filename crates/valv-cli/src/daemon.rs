use anyhow::{anyhow, Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::paths::socket_path;

const DAEMON_NOT_RUNNING: &str = "Daemon is not running. Start it with: valv daemon install";

pub(crate) fn daemon_client() -> Result<reqwest::Client> {
    let path = socket_path().context("failed to determine daemon socket path")?;
    if !path.exists() {
        return Err(anyhow!(DAEMON_NOT_RUNNING));
    }
    Ok(reqwest::Client::builder()
        .unix_socket(path)
        .build()
        .context("failed to create daemon HTTP client")?)
}

pub(crate) async fn parse_daemon_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T> {
    if !response.status().is_success() {
        return Err(anyhow!(daemon_error_message(response).await));
    }
    Ok(response
        .json::<T>()
        .await
        .context("failed to parse daemon response")?)
}

pub(crate) async fn expect_status(response: reqwest::Response, expected: StatusCode) -> Result<()> {
    if response.status() == expected {
        return Ok(());
    }
    Err(anyhow!(daemon_error_message(response).await))
}

async fn daemon_error_message(response: reqwest::Response) -> String {
    let status = response.status();
    match response.text().await {
        Ok(text) if !text.trim().is_empty() => format!("daemon returned {status}: {text}"),
        _ => format!("daemon returned {status}"),
    }
}

pub(crate) fn map_daemon_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_connect() {
        anyhow!(DAEMON_NOT_RUNNING)
    } else {
        error.into()
    }
}
