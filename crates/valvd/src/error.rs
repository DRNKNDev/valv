use std::fmt;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use valv_sync::sync_engine::update_required::{is_update_required, UpdateRequired};

#[derive(Debug)]
pub(crate) enum DaemonError {
    BadRequest(String),
    NotFound(String),
    Conflict(Value),
    Backend { status: StatusCode, body: Value },
    UpdateRequired(UpdateRequired),
    Internal(String),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::BadRequest(message)
            | DaemonError::NotFound(message)
            | DaemonError::Internal(message) => f.write_str(message),
            DaemonError::Conflict(body) => write!(f, "{body}"),
            DaemonError::Backend { status, body } => write!(f, "backend returned {status}: {body}"),
            DaemonError::UpdateRequired(update_required) => write!(f, "{update_required}"),
        }
    }
}

impl std::error::Error for DaemonError {}

impl IntoResponse for DaemonError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            DaemonError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, json!({ "error": message }))
            }
            DaemonError::NotFound(message) => (StatusCode::NOT_FOUND, json!({ "error": message })),
            DaemonError::Conflict(body) => (StatusCode::CONFLICT, body),
            DaemonError::Backend { status, body } => (status, body),
            DaemonError::UpdateRequired(update_required) => (
                StatusCode::UPGRADE_REQUIRED,
                json!({
                    "error": "update_required",
                    "min_protocol": update_required.min_protocol,
                    "message": update_required.message,
                }),
            ),
            DaemonError::Internal(message) => {
                tracing::error!(error = %message, "internal daemon error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": message }),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}

impl From<anyhow::Error> for DaemonError {
    fn from(error: anyhow::Error) -> Self {
        if let Some(update_required) = is_update_required(&error) {
            return DaemonError::UpdateRequired(update_required.clone());
        }
        DaemonError::Internal(error.to_string())
    }
}

impl From<reqwest::Error> for DaemonError {
    fn from(error: reqwest::Error) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

impl From<reqwest::header::InvalidHeaderName> for DaemonError {
    fn from(error: reqwest::header::InvalidHeaderName) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

impl From<reqwest::header::InvalidHeaderValue> for DaemonError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

impl From<rusqlite::Error> for DaemonError {
    fn from(error: rusqlite::Error) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

impl From<serde_json::Error> for DaemonError {
    fn from(error: serde_json::Error) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(error: std::io::Error) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

impl From<tokio::task::JoinError> for DaemonError {
    fn from(error: tokio::task::JoinError) -> Self {
        DaemonError::Internal(error.to_string())
    }
}

pub(crate) async fn backend_response_or_error(
    response: reqwest::Response,
) -> Result<reqwest::Response, DaemonError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }));
    Err(DaemonError::Backend { status, body })
}

#[cfg(test)]
mod tests {
    use axum::{body, response::IntoResponse};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    async fn response_parts(error: DaemonError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice::<Value>(&bytes).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn daemon_error_variants_map_to_status_and_json_body() {
        let cases = [
            (
                DaemonError::BadRequest("bad input".to_owned()),
                StatusCode::BAD_REQUEST,
                json!({ "error": "bad input" }),
            ),
            (
                DaemonError::NotFound("missing".to_owned()),
                StatusCode::NOT_FOUND,
                json!({ "error": "missing" }),
            ),
            (
                DaemonError::Conflict(json!({ "error": "superseded" })),
                StatusCode::CONFLICT,
                json!({ "error": "superseded" }),
            ),
            (
                DaemonError::Backend {
                    status: StatusCode::PAYMENT_REQUIRED,
                    body: json!({ "error": "subscription_inactive", "status": "none" }),
                },
                StatusCode::PAYMENT_REQUIRED,
                json!({ "error": "subscription_inactive", "status": "none" }),
            ),
            (
                DaemonError::Internal("db failed".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "db failed" }),
            ),
        ];

        for (error, expected_status, expected_body) in cases {
            let (status, body) = response_parts(error).await;
            assert_eq!(status, expected_status);
            assert_eq!(body, expected_body);
        }
    }

    #[tokio::test]
    async fn backend_response_or_error_preserves_json_status_and_body() {
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
        let error = backend_response_or_error(response).await.unwrap_err();

        match error {
            DaemonError::Backend { status, body } => {
                assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
                assert_eq!(
                    body,
                    json!({ "error": "subscription_inactive", "status": "none" })
                );
            }
            other => panic!("expected backend error, got {other:?}"),
        }
    }
}
