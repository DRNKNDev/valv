export type RequestApp = {
  request: (path: string, init?: RequestInit) => Response | Promise<Response>;
};

export type BareHarness = {
  createApp(): Promise<{ app: RequestApp; cleanup: () => Promise<void> }>;
};

export type SeedContext = {
  cookie: string;
  userId: string;
  deviceId: string;
  token: string;
  folderId: string;
  rootNodeId: string;
};

export type SeededHarness = {
  createApp(): Promise<{
    app: RequestApp;
    context: SeedContext;
    db: unknown;
    s3: unknown;
    bucket: string;
    row<T = Record<string, unknown>>(sql: string, ...params: unknown[]): Promise<T | undefined>;
    rows<T = Record<string, unknown>>(sql: string, ...params: unknown[]): Promise<T[]>;
    exec(sql: string, ...params: unknown[]): Promise<void>;
    cleanup(): Promise<void>;
  }>;
};
