use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountStatus {
    pub path: String,
    pub folder_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    pub syncing: bool,
    pub pending_ops: u64,
    pub last_synced_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub paused: bool,
    pub backend_connected: bool,
    pub version: String,
    pub mounts: Vec<MountStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountRequest {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountResponse {
    pub folder_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_node_id: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSummary {
    pub creates_submitted: u64,
    pub versions_submitted: u64,
    pub pulled_ops: i64,
    pub errors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version_id: String,
    pub created_at: String,
    pub size_bytes: u64,
    pub author_device_name: String,
    pub is_conflict_copy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionsRequest {
    pub local_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionsResponse {
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreRequest {
    pub local_path: String,
    pub version_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResponse {
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpItem {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub version_id: Option<String>,
    pub content_hash: Option<String>,
    pub size_bytes: Option<u64>,
    pub server_seq: i64,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpEnumerateQuery {
    pub parent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpEnumerateResponse {
    pub items: Vec<FpItem>,
    pub total: u64,
    pub synced_to_seq: i64,
    pub can_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpAnchorResponse {
    pub server_seq: i64,
    pub can_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpChangesResponse {
    pub items: Vec<FpItem>,
    pub current_seq: i64,
    pub more_coming: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpChunkDownload {
    pub chunk_hash: String,
    pub offset: u64,
    pub length: u64,
    pub url: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpContentResponse {
    pub version_id: String,
    pub size_bytes: u64,
    pub chunks: Vec<FpChunkDownload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpUploadRequest {
    pub node_id: Option<String>,
    pub parent_id: String,
    pub name: String,
    pub based_on_seq: Option<i64>,
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpUploadQueued {
    pub queued: bool,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpDeleteRequest {
    pub node_id: String,
    pub based_on_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpShareRequest {
    pub node_id: String,
    pub invited_email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FpShareResponse {
    pub invite_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePathResponse {
    pub path: String,
}
