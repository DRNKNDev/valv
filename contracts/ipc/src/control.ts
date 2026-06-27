export interface MountStatus {
  path: string;
  folder_id: string;
  grant_id?: string;
  syncing: boolean;
  pending_ops: number;
  last_synced_at: string | null;
  error?: string;
}

export interface DaemonStatus {
  paused: boolean;
  backend_connected: boolean;
  mounts: MountStatus[];
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

export interface SyncRequest {
  folder_id?: string;
}
