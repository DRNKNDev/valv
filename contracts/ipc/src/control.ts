export interface MountStatus {
  path: string;
  folder_id: string;
  name: string;
  scope_node_id?: string;
  grant_id?: string;
  // Was already tracked internally (used by GET /fp/items, GET /fp/anchor) but never
  // surfaced here - needed so a client (the macOS menu bar) can badge a read-only
  // mount without a per-mount extra call.
  can_write: boolean;
  syncing: boolean;
  pending_ops: number;
  last_synced_at: string | null;
  error?: string;
}

export interface AccountStatus {
  plan: string | null;
  status: string;
  usage_bytes: number;
  quota_bytes: number | null;
  current_period_end: string | null;
}

export interface DaemonStatus {
  paused: boolean;
  backend_connected: boolean;
  version: string;
  mounts: MountStatus[];
  account?: AccountStatus;
}

export interface NodePathResponse {
  path: string;
}

export interface MountRequest {
  path: string;
  folder_id?: string;
  grant_token?: string;
}

export interface MountResponse {
  folder_id: string;
  grant_id?: string;
  path: string;
}

// Unmounts locally only - does not touch the backend folder/grants, and does not
// delete the locally materialized files.
export interface UnmountRequest {
  folder_id: string;
}

export interface SyncRequest {
  folder_id?: string;
}
