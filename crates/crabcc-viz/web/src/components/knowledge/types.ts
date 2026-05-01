// Wire types for /api/memory/graph + /api/memory/get.
//
// Kept hand-typed (not aliased from api.gen.ts) for the same reason as
// the call-graph viewer: the OpenAPI declares these endpoints with
// `additionalProperties: true` so the generated alias is `unknown`.
// A typed wire surface here keeps the rest of the module strict.

export interface KnowledgeNode {
  id: string;
  title: string;
  kind: string;
  ts: number;
  len: number;
}

export interface KnowledgeEdge {
  src: string;
  dst: string;
  via: "ref" | "wiki" | string;
}

export interface KnowledgeStats {
  drawers: number;
  edges: number;
  embeddings: boolean;
}

export interface KnowledgeSnapshot {
  nodes: KnowledgeNode[];
  edges: KnowledgeEdge[];
  stats: KnowledgeStats;
}

export interface DrawerDetail {
  found: boolean;
  id: string;
  wing: string;
  room: string | null;
  source_id: string;
  body: string;
  created_at: number;
}
