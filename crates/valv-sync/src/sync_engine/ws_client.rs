use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
    sync::mpsc,
    time::{sleep, Duration},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::protocol::sync::WsPushNotification;

pub async fn ws_push_loop(
    backend_url: &str,
    token: &str,
    folder_ids: Vec<String>,
    tx: mpsc::Sender<WsPushNotification>,
) -> Result<()> {
    let ws_url = derive_ws_url(backend_url, token)?;

    loop {
        match connect_and_forward(&ws_url, &folder_ids, tx.clone()).await {
            Ok(()) => {}
            Err(err) => eprintln!("websocket disconnected: {err}"),
        }
        sleep(Duration::from_secs(2)).await;
    }
}

pub fn derive_ws_url(backend_url: &str, token: &str) -> Result<String> {
    let trimmed = backend_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_owned()
    } else {
        return Err(anyhow!("backend_url must start with http:// or https://"));
    };

    Ok(format!("{ws_base}/ws?token={token}"))
}

async fn connect_and_forward(
    ws_url: &str,
    folder_ids: &[String],
    tx: mpsc::Sender<WsPushNotification>,
) -> Result<()> {
    let (mut socket, _) = connect_async(ws_url).await?;
    for folder_id in folder_ids {
        socket
            .send(Message::Text(
                json!({"type":"subscribe","folder_id":folder_id}).to_string(),
            ))
            .await?;
    }

    while let Some(message) = socket.next().await {
        let message = message?;
        let Ok(text) = message.into_text() else {
            eprintln!("ignoring non-text websocket message");
            continue;
        };
        match serde_json::from_str::<WsPushNotification>(&text) {
            Ok(notification) => {
                if tx.send(notification).await.is_err() {
                    return Ok(());
                }
            }
            Err(err) => eprintln!("ignoring malformed websocket message: {err}"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_backend_derives_wss_url() {
        assert_eq!(
            derive_ws_url("https://api.valv.dev", "secret").unwrap(),
            "wss://api.valv.dev/ws?token=secret"
        );
    }

    #[test]
    fn http_backend_derives_ws_url() {
        assert_eq!(
            derive_ws_url("http://localhost:3000", "secret").unwrap(),
            "ws://localhost:3000/ws?token=secret"
        );
    }
}
