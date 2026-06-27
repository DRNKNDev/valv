import type { OpType } from './ops';

export interface OpLogEntry {
  server_seq: number;
  node_id: string;
  op_type: OpType;
  op_payload: Record<string, unknown>;
  actor_device_id: string;
  applied_at: string;
}

export interface DeltaPullResponse {
  ops: OpLogEntry[];
  up_to_seq: number;
}

export interface NodeSnapshot {
  node_id: string;
  parent_id: string | null;
  name: string;
  type: 'file' | 'folder';
  current_version_id: string | null;
  server_seq: number;
  deleted_at: string | null;
}

export interface FolderTreeResponse {
  nodes: NodeSnapshot[];
  up_to_seq: number;
}
