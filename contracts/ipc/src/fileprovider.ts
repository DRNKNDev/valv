export interface FpItem {
  node_id: string;
  parent_id: string | null;
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
  offset?: number;
  limit?: number;
}

export interface FpEnumerateResponse {
  items: FpItem[];
  total: number;
  synced_to_seq: number;
}

export interface FpAnchorResponse {
  server_seq: number;
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
