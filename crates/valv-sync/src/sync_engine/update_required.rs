use anyhow::{anyhow, Error};
use reqwest::StatusCode;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("update required: {message}")]
pub struct UpdateRequired {
    pub min_protocol: Option<i64>,
    pub message: String,
}

impl UpdateRequired {
    pub fn unrecognized_submit_result(result: Option<&str>) -> Self {
        let detail = result
            .map(|result| format!("unrecognized op submission result `{result}`"))
            .unwrap_or_else(|| "missing op submission result".to_owned());
        Self {
            min_protocol: None,
            message: format!("{detail}; update Valv to keep syncing"),
        }
    }

    pub fn unrecognized_op_type(op_type: &str) -> Self {
        Self {
            min_protocol: None,
            message: format!("unrecognized op type `{op_type}`; update Valv to keep syncing"),
        }
    }

    pub fn protocol_too_old(min_protocol: Option<i64>, message: Option<String>) -> Self {
        Self {
            min_protocol,
            message: message.unwrap_or_else(|| "Update Valv to keep syncing.".to_owned()),
        }
    }

    pub fn mount_already_halted() -> Self {
        Self {
            min_protocol: None,
            message: "Update Valv to keep syncing.".to_owned(),
        }
    }
}

pub fn is_update_required(error: &Error) -> Option<&UpdateRequired> {
    error.downcast_ref::<UpdateRequired>()
}

#[derive(Debug, Deserialize)]
struct ProtocolTooOldBody {
    min_protocol: Option<i64>,
    message: Option<String>,
}

pub async fn update_required_from_response(response: reqwest::Response) -> Error {
    let status = response.status();
    if status == StatusCode::UPGRADE_REQUIRED {
        let body = response.json::<ProtocolTooOldBody>().await.ok();
        return anyhow!(UpdateRequired::protocol_too_old(
            body.as_ref().and_then(|body| body.min_protocol),
            body.and_then(|body| body.message),
        ));
    }
    anyhow!("unexpected update-required response status {status}")
}
