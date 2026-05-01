// Activity-panel-internal types. The wire shape is `ActivityHit` from
// the OpenAPI codegen — these are the *display* models we derive from
// it (group rows, time-bucket headers, virtualized rows).

import type { ActivityHit } from "../../api";

export type { ActivityHit };

/// A row in the rendered list. Either a literal hit, a "group" with N
/// hits collapsed under one op, or a sticky time-bucket header.
export type Row =
  | { kind: "header"; key: string; label: string }
  | { kind: "hit"; key: string; hit: ActivityHit; pinned: boolean }
  | {
      kind: "group";
      key: string;
      op: string;
      count: number;
      lastQuery: string;
      lastTs: number;
      hits: ActivityHit[];
      expanded: boolean;
    };

export interface FilterState {
  text: string; // case-insensitive substring (matches query)
  op: string | null; // exact op match (e.g. "sym")
  agent: string | null; // exact agent id match
}

export const EMPTY_FILTER: FilterState = { text: "", op: null, agent: null };
