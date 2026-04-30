// Protocol shapes shared by service-worker, offscreen, and content scripts.
// Mirrors the JSON-RPC envelope the `crabcc serve` chrome-bridge endpoints
// will emit (downlink) and accept (uplink). Will be replaced by typeshare-
// generated types once the Rust broker module lands in crabcc-viz.

export type JsonRpcId = string | number;

export interface JsonRpcRequest<P = unknown> {
  jsonrpc: "2.0";
  id: JsonRpcId;
  method: string;
  params?: P;
}

export interface JsonRpcSuccess<R = unknown> {
  jsonrpc: "2.0";
  id: JsonRpcId;
  result: R;
}

export interface JsonRpcError {
  jsonrpc: "2.0";
  id: JsonRpcId;
  error: { code: number; message: string; data?: unknown };
}

export type JsonRpcResponse<R = unknown> = JsonRpcSuccess<R> | JsonRpcError;

export const RPC_ERROR = {
  PARSE: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL: -32603,
} as const;

export interface PageListPagesResult {
  tabs: Array<{ id: number; url: string; title: string; active: boolean }>;
}
