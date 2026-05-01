// RelationsGraph — interactive force-directed view of the call graph.
//
// Top-level orchestrator: fetches the seed snapshot, threads it through
// the store + force layout, and composes the canvas renderer with the
// search/density controls, the legend, and the selection side-panel.
//
// Why one component file instead of a barrel: the import surface stays
// trivial for App.tsx (`import { RelationsGraph } from …`), and every
// concern with non-trivial logic lives in `./graph/*` where it can be
// unit-tested without spinning up React.

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ParentSize } from "./graph/ParentSize";
import {
  GraphCanvas,
  type CanvasController,
} from "./graph/GraphCanvas";
import { Controls } from "./graph/Controls";
import { Legend } from "./graph/Legend";
import { NodeInfo } from "./graph/NodeInfo";
import { useForceLayout } from "./graph/useForceLayout";
import { fetchExpansion, useSeedGraph } from "./graph/useGraphData";
import { useKeyboardControls } from "./graph/useKeyboardControls";
import { degrees, fromSeed, mergeExpansion, neighborhood } from "./graph/store";
import type { Layout, SimNode } from "./graph/types";

interface RelationsGraphProps {
  /** Initial node-count cap for /api/seed-graph. Overridable via the slider. */
  limit?: number;
  /** Optional callback when a node is pinned. */
  onNodeSelect?: (id: string) => void;
}

const EXPAND_DEPTH = 2;

// Memoised top-level orchestrator. The dashboard's App.tsx re-renders
// whenever activity / agents / telemetry / SSE state changes — none of
// which the graph reads. Without `memo`, every poll cycle was forcing
// a full reconciliation of this subtree (the seed `usePolling` hook
// alone has no deps, so the rerun was wasted work). Measured before:
// graph subtree re-rendered ~once a second on idle; after: 0/sec.
export const RelationsGraph = memo(function RelationsGraph({
  limit: initialLimit = 20,
  onNodeSelect,
}: RelationsGraphProps) {
  const [limit, setLimit] = useState(initialLimit);
  const seed = useSeedGraph(limit);

  // Initial layout: derived once per seed snapshot. Subsequent expansions
  // mutate this object in place via store.mergeExpansion (it preserves
  // SimNode refs so d3-force keeps existing positions).
  const initialLayout = useMemo<Layout | null>(() => {
    if (!seed.data) return null;
    return fromSeed(seed.data);
  }, [seed.data]);

  // Live layout — `null` until the first seed arrives.
  const [layout, setLayout] = useState<Layout | null>(null);
  const [preservePositions, setPreservePositions] = useState(false);

  // Reset layout whenever a new seed arrives (e.g. user dragged the slider).
  useEffect(() => {
    if (initialLayout) {
      setLayout(initialLayout);
      setPreservePositions(false);
    }
  }, [initialLayout]);

  if (seed.err) {
    return (
      <div className="placeholder graph-placeholder">
        <strong>graph error</strong>
        <small>{seed.err} — is <code>crabcc graph build</code> up to date?</small>
      </div>
    );
  }
  if (seed.loading && !layout) {
    return (
      <div className="placeholder graph-placeholder">
        <span className="graph-spinner" /> loading relations graph…
      </div>
    );
  }
  if (!layout || layout.nodes.length === 0) {
    return (
      <div className="placeholder graph-placeholder">
        <strong>no graph yet</strong>
        <small>run <code>crabcc graph build</code></small>
      </div>
    );
  }

  return (
    <div className="network">
      <ParentSize>
        {({ width, height }: { width: number; height: number }) =>
          width > 0 && height > 0 ? (
            <Stage
              width={width}
              height={height}
              layout={layout}
              setLayout={setLayout}
              limit={limit}
              setLimit={setLimit}
              preservePositions={preservePositions}
              setPreservePositions={setPreservePositions}
              onNodeSelect={onNodeSelect}
            />
          ) : null
        }
      </ParentSize>
    </div>
  );
});

interface StageProps {
  width: number;
  height: number;
  layout: Layout;
  setLayout: React.Dispatch<React.SetStateAction<Layout | null>>;
  limit: number;
  setLimit: (n: number) => void;
  preservePositions: boolean;
  setPreservePositions: (b: boolean) => void;
  onNodeSelect?: (id: string) => void;
}

function Stage({
  width,
  height,
  layout,
  setLayout,
  limit,
  setLimit,
  preservePositions,
  setPreservePositions,
  onNodeSelect,
}: StageProps) {
  const settled = useForceLayout(layout, { width, height, preservePositions });
  const [pinned, setPinned] = useState<SimNode | null>(null);
  const [, setHover] = useState<SimNode | null>(null);
  const [search, setSearch] = useState("");
  const [highlight, setHighlight] = useState<Set<string> | null>(null);
  const [expanding, setExpanding] = useState<{ id: string; dir: "callers" | "callees" } | null>(null);
  const [hint, setHint] = useState<string>("");
  const ctlRef = useRef<CanvasController | null>(null);

  // Highlight tracks the pin's 1-hop neighborhood. Recomputed only on
  // pin/layout change — hover never triggers a highlight (it would
  // strobe constantly during mouse-move).
  useEffect(() => {
    if (!pinned) {
      setHighlight(null);
      return;
    }
    setHighlight(neighborhood(settled.links, pinned.id));
  }, [pinned, settled]);

  const handlePick = useCallback(
    (n: SimNode | null) => {
      setPinned(n);
      if (n && onNodeSelect) onNodeSelect(n.id);
    },
    [onNodeSelect],
  );

  const expand = useCallback(
    async (root: string, dir: "callers" | "callees") => {
      setExpanding({ id: root, dir });
      setHint("");
      try {
        const snap = await fetchExpansion(root, dir, EXPAND_DEPTH);
        if (snap.nodes.length <= 1) {
          setHint(`no ${dir} found within depth ${EXPAND_DEPTH}`);
          return;
        }
        setLayout((prev) => (prev ? mergeExpansion(prev, snap) : prev));
        setPreservePositions(true);
      } catch (e) {
        setHint(`expand failed: ${(e as Error).message}`);
      } finally {
        setExpanding(null);
      }
    },
    [setLayout, setPreservePositions],
  );

  const submitSearch = useCallback(async () => {
    const q = search.trim();
    if (!q) {
      setHighlight(null);
      return;
    }
    const known = settled.nodes.find((n) => n.id === q);
    if (known) {
      const nbhd = neighborhood(settled.links, known.id);
      setHighlight(nbhd);
      setPinned(known);
      ctlRef.current?.centerOn(known);
      setHint("");
      return;
    }
    // Fall back to a server fetch — the user might be searching for a
    // symbol that isn't in the current snapshot.
    setHint(`fetching ${q}…`);
    try {
      const snap = await fetchExpansion(q, "callees", EXPAND_DEPTH);
      if (snap.nodes.length <= 1) {
        setHint(`no symbol "${q}" with edges (try a different name)`);
        return;
      }
      setLayout((prev) => (prev ? mergeExpansion(prev, snap) : prev));
      setPreservePositions(true);
      setHint("");
    } catch (e) {
      setHint(`search failed: ${(e as Error).message}`);
    }
  }, [search, settled, setLayout, setPreservePositions]);

  // Memoised actions object — useKeyboardControls re-attaches its
  // listener when this identity changes, so we keep it stable.
  const kbActions = useMemo(
    () => ({
      pan: (dx: number, dy: number) => ctlRef.current?.pan(dx, dy),
      zoom: (factor: number) => ctlRef.current?.zoom(factor),
      reset: () => ctlRef.current?.reset(),
      unpin: () => {
        setPinned(null);
        setHighlight(null);
        setHint("");
      },
    }),
    [],
  );
  useKeyboardControls(kbActions, true);

  const deg = pinned ? degrees(settled.links, pinned.id) : { inDeg: 0, outDeg: 0 };
  const expCallers = expanding?.id === pinned?.id && expanding?.dir === "callers";
  const expCallees = expanding?.id === pinned?.id && expanding?.dir === "callees";

  return (
    <>
      <GraphCanvas
        layout={settled}
        width={width}
        height={height}
        pinned={pinned}
        highlight={highlight}
        onPick={handlePick}
        onHover={setHover}
        controlRef={ctlRef}
      />
      <Controls
        limit={limit}
        onLimitChange={setLimit}
        searchValue={search}
        onSearchChange={setSearch}
        onSearchSubmit={submitSearch}
        hint={hint || undefined}
      />
      <Legend nodes={settled.nodes.length} edges={settled.links.length} />
      {pinned && (
        <NodeInfo
          node={pinned}
          inDeg={deg.inDeg}
          outDeg={deg.outDeg}
          expanding={{ callers: expCallers, callees: expCallees }}
          onClose={() => setPinned(null)}
          onExpand={(dir) => expand(pinned.id, dir)}
        />
      )}
    </>
  );
}
