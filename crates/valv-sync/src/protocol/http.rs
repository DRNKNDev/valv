use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchOperation {
    Upload,
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRequestObject {
    pub oid: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRequest {
    pub operation: BatchOperation,
    pub transfer: String,
    pub objects: Vec<BatchRequestObject>,
}

impl BatchRequest {
    pub fn new(operation: BatchOperation, objects: Vec<BatchRequestObject>) -> Self {
        Self {
            operation,
            transfer: "basic".into(),
            objects,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchAction {
    pub href: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchActions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<BatchAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<BatchAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchObjectError {
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchResponseObject {
    pub oid: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<BatchActions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BatchObjectError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchResponse {
    pub transfer: String,
    pub objects: Vec<BatchResponseObject>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_actions_deserializes_as_none() {
        let object: BatchResponseObject =
            serde_json::from_str(r#"{"oid":"abc123","size":1024}"#).unwrap();

        assert!(object.actions.is_none());
    }
}
