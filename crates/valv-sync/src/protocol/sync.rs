use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePayload {
    pub node_id: String,
    pub parent_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePayload {
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovePayload {
    pub new_parent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePayload {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub chunk_hash: String,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewVersionPayload {
    pub version_id: String,
    pub content_hash: String,
    pub size_bytes: u64,
    pub manifest: Vec<ChunkRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op_type", rename_all = "snake_case")]
pub enum SubmitOpRequest {
    Create {
        payload: CreatePayload,
    },
    Rename {
        node_id: String,
        based_on_seq: i64,
        payload: RenamePayload,
    },
    Move {
        node_id: String,
        based_on_seq: i64,
        payload: MovePayload,
    },
    Delete {
        node_id: String,
        based_on_seq: i64,
        payload: DeletePayload,
    },
    NewVersion {
        node_id: String,
        based_on_seq: i64,
        payload: NewVersionPayload,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SubmitOpResponse {
    Applied {
        server_seq: i64,
        node_id: String,
    },
    ConflictCopy {
        server_seq: i64,
        node_id: String,
        conflict_version_id: String,
    },
    Superseded {
        current_seq: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpLogEntry {
    pub server_seq: i64,
    pub node_id: String,
    pub op_type: String,
    pub op_payload: serde_json::Value,
    pub actor_device_id: String,
    pub applied_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaPullResponse {
    pub ops: Vec<OpLogEntry>,
    pub up_to_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub current_version_id: Option<String>,
    pub server_seq: i64,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderTreeResponse {
    pub nodes: Vec<NodeSnapshot>,
    pub up_to_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsPushNotification {
    pub folder_id: String,
    pub server_seq: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_request_serializes_without_node_id() {
        let req = SubmitOpRequest::Create {
            payload: CreatePayload {
                node_id: "node-1".into(),
                parent_id: "root".into(),
                name: "report.md".into(),
                node_type: NodeType::File,
            },
        };

        let json = serde_json::to_value(req).unwrap();

        assert_eq!(json["op_type"], "create");
        assert!(json.get("node_id").is_none());
        assert!(json.get("based_on_seq").is_none());
        assert_eq!(json["payload"]["node_id"], "node-1");
    }

    #[test]
    fn ws_push_notification_serializes_to_exactly_two_fields() {
        let notification = WsPushNotification {
            folder_id: "f1".into(),
            server_seq: 99,
        };

        let json = serde_json::to_value(notification).unwrap();
        let obj = json.as_object().unwrap();

        assert_eq!(obj.len(), 2);
        assert_eq!(json["folder_id"], "f1");
        assert_eq!(json["server_seq"], 99);
    }
}
