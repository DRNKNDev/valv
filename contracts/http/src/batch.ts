export type BatchOperation = 'upload' | 'download';

export interface BatchRequestObject {
  oid: string;
  size: number;
}

export interface BatchRequest {
  operation: BatchOperation;
  objects: BatchRequestObject[];
}

export interface BatchAction {
  href: string;
  header?: Record<string, string>;
  expires_in?: number;
}

export interface BatchResponseObject {
  oid: string;
  size: number;
  actions?: {
    upload?: BatchAction;
    download?: BatchAction;
  };
  error?: {
    code: number;
    message: string;
  };
}

export interface BatchResponse {
  transfer: 'basic';
  objects: BatchResponseObject[];
}
