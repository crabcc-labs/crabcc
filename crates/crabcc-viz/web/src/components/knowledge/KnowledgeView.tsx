// KnowledgeView — top-level orchestrator for the #/knowledge route.
//
// Reuses the call-graph viewer's primitives:
//   - GraphCanvas (rendering, pan/zoom/hit-test)
//   - useForceLayout (d3-force settle)
// The wire shape is different (memory drawers, not symbol calls), so
// the data fetch + side panel + empty-state live in this module. The
// transformations between the wire shape and the SimNode/SimLink that
// GraphCanvas expects are pure functions in `./store.ts`.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Search } from "lucide-react";
import { Icon } from "../icons";
import { ParentSize } from "../graph/ParentSize";
import {
  GraphCanvas,
  type CanvasController,
} from "../graph/GraphCanvas";
import { useForceLayout } from "../graph/useForceLayout";
import { useKeyboardControls } from "../graph/useKeyboardControls";
import type { Layout, SimNode } from "../graph/types";
import { useKnowledgeGraph } from "./useKnowledgeData";
import {
  fromSnapshot,
  inferSeeds,
  neighborhoodOf,
  searchHighlight,
  titleIndex,
} from "./store";
import { DrawerPanel } from "./DrawerPanel";
import { EmptyState } from "./EmptyState";
import { IngestBox } from "./IngestBox";
import { logFetchOk } from "../../lifecycle";

const DEFAULT_LIMIT = 200;

export function KnowledgeView() {
  const [limit] = useState(DEFAULT_LIMIT);
  const snap = useKnowledgeGraph(limit);

  // Lifted up so the IngestBox can drive selection from outside the
  // graph stage. `selectId` survives a refetch — the side panel
  // re-fetches the body when the snapshot reloads with the new drawer.
  const [selectId, setSelectId] = useState<string | null>(null);

  const layout = useMemo<Layout | null>(() => {
    if (!snap.data) return null;
    return fromSnapshot(snap.data);
  }, [snap.data]);

  const titles = useMemo(() => {
    if (!snap.data) return new Map<string, string>();
    return titleIndex(snap.data);
  }, [snap.data]);

  // Surface load events to the lifecycle log so the dev console has
  // breadcrumbs for "knowledge view ready".
  useEffect(() => {
    if (snap.data) {
      logFetchOk(
        "knowledge:graph",
        `${snap.data.stats.drawers} drawers · ${snap.data.stats.edges} edges`,
      );
    }
  }, [snap.data]);

  const onIngested = useCallback(() => {
    // The user just persisted new drawers — pull the graph again so
    // the canvas reflects them. We don't auto-pin a node here; the
    // result card lets the user pick which one to inspect.
    snap.refetch();
  }, [snap]);

  // The shell (App.tsx) renders the global Header + layout chrome — this
  // view just produces the `<main>` content. That keeps a single source
  // of truth for the nav strip + active-route highlight.

  if (snap.err) {
    return (
      <main className="knowledge-main">
        <IngestBox onIngested={onIngested} onSelect={setSelectId} />
        <div className="placeholder">
          <strong>memory graph error</strong>
          <small>{snap.err}</small>
        </div>
      </main>
    );
  }

  if (snap.loading && !layout) {
    return (
      <main className="knowledge-main">
        <IngestBox onIngested={onIngested} onSelect={setSelectId} />
        <div className="placeholder">
          <span className="graph-spinner" /> loading knowledge graph…
        </div>
      </main>
    );
  }

  if (!snap.data || snap.data.stats.drawers === 0) {
    return (
      <main className="knowledge-main">
        <IngestBox onIngested={onIngested} onSelect={setSelectId} />
        <EmptyState />
      </main>
    );
  }

  if (!layout) return null;

  return (
    <main className="knowledge-main">
      <IngestBox onIngested={onIngested} onSelect={setSelectId} />
      <KnowledgeStage
        layout={layout}
        titles={titles}
        stats={snap.data.stats}
        selectId={selectId}
        onClearSelect={() => setSelectId(null)}
      />
    </main>
  );
}

interface StageProps {
  layout: Layout;
  titles: Map<string, string>;
  stats: { drawers: number; edges: number; embeddings: boolean };
  selectId: string | null;
  onClearSelect: () => void;
}

function KnowledgeStage(props: StageProps) {
  return (
    <section className="knowledge-stage">
      <ParentSize>
        {({ width, height }: { width: number; height: number }) =>
          width > 0 && height > 0 ? (
            <Inner width={width} height={height} {...props} />
          ) : null
        }
      </ParentSize>
    </section>
  );
}

interface InnerProps extends StageProps {
  width: number;
  height: number;
}

function Inner({ width, height, layout, titles, stats, selectId, onClearSelect }: InnerProps) {
  const settled = useForceLayout(layout, { width, height, preservePositions: false });
  const [pinned, setPinned] = useState<SimNode | null>(null);
  const [, setHover] = useState<SimNode | null>(null);
  const [search, setSearch] = useState("");
  const [highlight, setHighlight] = useState<Set<string> | null>(null);
  const ctlRef = useRef<CanvasController | null>(null);

  // External pin requests (e.g. from the IngestBox result card) feed
  // into the same `pinned` state the canvas drives. We resolve the
  // id → node here because the parent doesn't know about SimNodes.
  useEffect(() => {
    if (!selectId) return;
    const node = settled.nodes.find((n) => n.id === selectId);
    if (node) setPinned(node);
  }, [selectId, settled.nodes]);

  // Sync highlight on either pin or search. Pin's 1-hop neighborhood
  // wins when both are set — the user just clicked a node, that's the
  // most-specific signal.
  useEffect(() => {
    if (pinned) {
      setHighlight(neighborhoodOf(settled.links, pinned.id));
      return;
    }
    setHighlight(searchHighlight(search, settled.nodes, titles));
  }, [pinned, search, settled, titles]);

  const handlePick = useCallback(
    (n: SimNode | null) => {
      setPinned(n);
      if (!n) onClearSelect();
    },
    [onClearSelect],
  );

  const kbActions = useMemo(
    () => ({
      pan: (dx: number, dy: number) => ctlRef.current?.pan(dx, dy),
      zoom: (factor: number) => ctlRef.current?.zoom(factor),
      reset: () => ctlRef.current?.reset(),
      unpin: () => {
        setPinned(null);
        onClearSelect();
        setSearch("");
      },
    }),
    [onClearSelect],
  );
  useKeyboardControls(kbActions, true);

  const seeds = useMemo(() => inferSeeds({
    nodes: layout.nodes.map((n) => ({ id: n.id, title: titles.get(n.id) ?? "", kind: "", ts: 0, len: 0 })),
    edges: layout.links.map((l) => ({ src: l.source.id, dst: l.target.id, via: "ref" as const })),
    stats,
  }), [layout, titles, stats]);

  return (
    <div className="knowledge-graph-area">
      <div className="knowledge-canvas-wrap">
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
        <div className="knowledge-overlay">
          <span className="knowledge-search-wrap">
            <Icon of={Search} size={12} className="knowledge-search-ico" aria-hidden="true" />
            <input
              type="text"
              className="knowledge-search"
              placeholder="search drawer title or id…"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              spellCheck={false}
              aria-label="Search knowledge graph"
            />
          </span>
          <span className="knowledge-meta">
            {settled.nodes.length} drawers · {settled.links.length} refs
            {stats.embeddings ? " · embeddings" : ""}
            {seeds.size > 0 ? ` · ${seeds.size} hubs` : ""}
          </span>
        </div>
      </div>
      <DrawerPanel
        id={pinned?.id ?? null}
        onClose={() => {
          setPinned(null);
          onClearSelect();
        }}
      />
    </div>
  );
}
