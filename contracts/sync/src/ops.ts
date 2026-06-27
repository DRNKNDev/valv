import type { ChunkRef } from './manifest';

export type OpType = 'create' | 'rename' | 'move' | 'delete' | 'new_version';

export interface CreatePayload {
  parent_id: string;
  name: string;
  type: 'file' | 'folder';
}

export interface RenamePayload {
  new_name: string;
}

export interface MovePayload {
  new_parent_id: string;
}

export interface DeletePayload {}

export interface NewVersionPayload {
  version_id: string;
  content_hash: string;
  size_bytes: number;
  manifest: ChunkRef[];
}

export type SubmitOpRequest =
  | {
      op_type: 'create';
      node_id?: never;
      based_on_seq?: never;
      payload: CreatePayload;
    }
  | {
      op_type: 'rename';
      node_id: string;
      based_on_seq: number;
      payload: RenamePayload;
    }
  | {
      op_type: 'move';
      node_id: string;
      based_on_seq: number;
      payload: MovePayload;
    }
  | {
      op_type: 'delete';
      node_id: string;
      based_on_seq: number;
      payload: DeletePayload;
    }
  | {
      op_type: 'new_version';
      node_id: string;
      based_on_seq: number;
      payload: NewVersionPayload;
    };

export type SubmitOpResponse =
  | {
      result: 'applied';
      server_seq: number;
      node_id: string;
    }
  | {
      result: 'conflict_copy';
      server_seq: number;
      node_id: string;
      conflict_version_id: string;
    }
  | {
      result: 'superseded';
      current_seq: number;
    };
