export interface FpItem {
  node_id: string;
  parent_id: string | null;
  // Lets a client resolve "which mount does this node belong to" from the item
  // itself, without a separate lookup or a client-maintained cache - needed once a
  // client (e.g. the macOS File Provider extension, phase-5-macos-gui) deals with
  // more than one mount at a time, since GET /fp/items/GET /fp/anchor/etc. all
  // require folder_id explicitly once more than one folder is mounted.
  folder_id: string;
  name: string;
  type: 'file' | 'folder';
  version_id: string | null;
  content_hash: string | null;
  size_bytes: number | null;
  server_seq: number;
  deleted: boolean;
}

export interface FpEnumerateQuery {
  parent: string;
  // Required once more than one folder is mounted (valvd's resolve_mount_for_query
  // returns an error otherwise) - optional only in the single-mount case. Previously
  // missing from this contract even though valvd's actual FpItemsQuery has always
  // accepted it, discovered while implementing the macOS GUI's synthetic multi-mount
  // root (phase-5-macos-gui), which always has more than one mount to disambiguate.
  folder_id?: string;
  offset?: number;
  limit?: number;
}

export interface FpEnumerateResponse {
  items: FpItem[];
  total: number;
  synced_to_seq: number;
  can_write: boolean;
}

export interface FpAnchorResponse {
  server_seq: number;
  can_write: boolean;
}

export interface FpChangesResponse {
  items: FpItem[];
  current_seq: number;
  more_coming: boolean;
}

export interface FpChunkDownload {
  chunk_hash: string;
  offset: number;
  length: number;
  url: string;
  expires_in: number;
}

export interface FpContentResponse {
  version_id: string;
  size_bytes: number;
  chunks: FpChunkDownload[];
}

export interface FpUploadRequest {
  node_id: string | null;
  parent_id: string;
  name: string;
  based_on_seq: number | null;
  file_path: string;
}

export interface FpUploadQueued {
  queued: true;
  node_id: string;
}

export interface FpDeleteRequest {
  node_id: string;
  based_on_seq: number;
}

export interface FpShareRequest {
  node_id: string;
  invited_email: string;
  // Defaults to true (read-write) when omitted - mirrors POST /folders/:id/invites's
  // own can_write default.
  can_write?: boolean;
}

export interface FpShareResponse {
  invite_url: string;
}

export interface FpWatchQuery {
  folder_id?: string;
  since_seq: number;
}

export type FpWatchResponse = FpAnchorResponse;
