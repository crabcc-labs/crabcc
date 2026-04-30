import { useEffect, useRef } from "react";

declare global {
  interface Window {
    vis?: {
      Network: new (
        container: HTMLElement,
        data: { nodes: VisNode[]; edges: VisEdge[] },
        options?: Record<string, unknown>,
      ) => VisNetwork;
    };
  }
}

type VisNode = { id: string; label: string };
type VisEdge = { from: string; to: string };
interface VisNetwork {
  destroy(): void;
  setData(data: { nodes: VisNode[]; edges: VisEdge[] }): void;
}

export type GraphSnapshot = {
  nodes: { id: string }[];
  edges: { src: string; dst: string }[];
};

/// Thin imperative wrapper around vis-network. We don't depend on
/// `react-vis-network` (unmaintained) — instead we attach the imperative
/// instance in `useEffect`, recreate it when the snapshot identity
/// changes, and tear it down on unmount.
///
/// vis-network is loaded via a `<script>` tag at runtime (lazy, only
/// when the graph mounts) to keep the React bundle small and avoid
/// pulling ~220 KB of layout engine into the initial parse.
export function Graph({ snapshot }: { snapshot: GraphSnapshot | null }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const networkRef = useRef<VisNetwork | null>(null);

  // Lazy-load vis-network the first time the component mounts.
  useEffect(() => {
    if (window.vis) return;
    const s = document.createElement("script");
    s.src = "https://unpkg.com/vis-network@9.1.13/standalone/umd/vis-network.min.js";
    s.async = true;
    document.head.appendChild(s);
    return () => {
      // Don't remove — keep cached for re-mounts.
    };
  }, []);

  useEffect(() => {
    if (!containerRef.current || !snapshot || !window.vis) return;
    const nodes: VisNode[] = snapshot.nodes.map((n) => ({ id: n.id, label: n.id }));
    const edges: VisEdge[] = snapshot.edges.map((e) => ({ from: e.src, to: e.dst }));
    networkRef.current?.destroy();
    networkRef.current = new window.vis.Network(
      containerRef.current,
      { nodes, edges },
      {
        physics: { stabilization: { iterations: 60 } },
        interaction: { hover: true, dragNodes: true },
        nodes: { shape: "dot", size: 12 },
      },
    );
    return () => {
      networkRef.current?.destroy();
      networkRef.current = null;
    };
  }, [snapshot]);

  if (!snapshot) {
    return <div className="placeholder">Loading seed graph…</div>;
  }
  return <div ref={containerRef} className="network" />;
}
