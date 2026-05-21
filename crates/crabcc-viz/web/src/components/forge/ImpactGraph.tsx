// PR blast-radius graph — shows which symbols are directly changed
// and which callers / callees are impacted. Built on top of the same
// d3-force layout used by the existing RelationsGraph.

import { useEffect, useRef, useState, memo } from "react";
import { cn } from "../../lib/cn";
import type { PrImpactGraph as PrImpactGraphData } from "../../api";

type Props = {
  data: PrImpactGraphData;
};

interface SimNode {
  id: string;
  label: string;
  kind: string;
  changed: boolean;
  depth: number;
  x?: number;
  y?: number;
  vx?: number;
  vy?: number;
  fx?: number | null;
  fy?: number | null;
}

interface SimLink {
  source: string | SimNode;
  target: string | SimNode;
}

function nodeColor(node: SimNode): string {
  if (node.changed) return "var(--color-primary)";
  if (node.depth === 1) return "var(--color-destructive)";
  return "var(--color-inactive)";
}

export const ImpactGraph = memo(function ImpactGraph({ data }: Props) {
  const svgRef = useRef<SVGSVGElement>(null);
  const [d3Ready, setD3Ready] = useState(false);

  useEffect(() => {
    const svg = svgRef.current;
    if (!svg || data.nodes.length === 0) return;

    const width = svg.clientWidth || 600;
    const height = svg.clientHeight || 360;

    // Dynamic import to keep initial bundle lean.
    Promise.all([
      import("d3-force"),
      import("d3-selection"),
      import("d3-zoom"),
    ]).then(([force, d3sel, zoom]) => {
      setD3Ready(true);
      const nodes: SimNode[] = data.nodes.map((n) => ({ ...n }));
      const links: SimLink[] = data.edges.map((e) => ({
        source: e.src,
        target: e.dst,
      }));

      // Clear previous render.
      d3sel.select(svg).selectAll("*").remove();

      const root = d3sel
        .select(svg)
        .append("g")
        .attr("class", "graph-root");

      // Arrow marker.
      d3sel
        .select(svg)
        .append("defs")
        .append("marker")
        .attr("id", "impact-arrow")
        .attr("viewBox", "0 -4 8 8")
        .attr("refX", 14)
        .attr("markerWidth", 6)
        .attr("markerHeight", 6)
        .attr("orient", "auto")
        .append("path")
        .attr("d", "M0,-4L8,0L0,4")
        .attr("fill", "var(--color-border)");

      const linkSel = root
        .append("g")
        .selectAll<SVGLineElement, SimLink>("line")
        .data(links)
        .join("line")
        .attr("stroke", "var(--color-border)")
        .attr("stroke-opacity", 0.6)
        .attr("stroke-width", 1.2)
        .attr("marker-end", "url(#impact-arrow)");

      const nodeSel = root
        .append("g")
        .selectAll<SVGGElement, SimNode>("g")
        .data(nodes)
        .join("g")
        .attr("cursor", "pointer");

      nodeSel
        .append("circle")
        .attr("r", (d) => (d.changed ? 8 : 5))
        .attr("fill", (d) => nodeColor(d))
        .attr("fill-opacity", (d) => (d.changed ? 1 : 0.7))
        .attr("stroke", "var(--color-card)")
        .attr("stroke-width", 1.5);

      nodeSel
        .append("text")
        .text((d) => d.label)
        .attr("dx", 10)
        .attr("dy", "0.35em")
        .attr("font-size", "10px")
        .attr("fill", "var(--color-foreground)")
        .attr("pointer-events", "none");

      // Tooltip title.
      nodeSel.append("title").text((d) => `${d.id}\n${d.file}${d.line ? `:${d.line}` : ""}`);

      const simulation = force
        .forceSimulation(nodes)
        .force(
          "link",
          force
            .forceLink<SimNode, SimLink>(links)
            .id((d) => d.id)
            .distance(80),
        )
        .force("charge", force.forceManyBody().strength(-120))
        .force("center", force.forceCenter(width / 2, height / 2))
        .force("x", force.forceX(width / 2).strength(0.05))
        .force("y", force.forceY(height / 2).strength(0.05));

      // Pin changed nodes to rough center-left.
      nodes.forEach((n, i) => {
        if (n.changed) {
          n.fx = width * 0.35 + (i % 4) * 20;
          n.fy = height * 0.3 + Math.floor(i / 4) * 40;
        }
      });

      simulation.on("tick", () => {
        linkSel
          .attr("x1", (d) => (d.source as SimNode).x ?? 0)
          .attr("y1", (d) => (d.source as SimNode).y ?? 0)
          .attr("x2", (d) => (d.target as SimNode).x ?? 0)
          .attr("y2", (d) => (d.target as SimNode).y ?? 0);
        nodeSel.attr("transform", (d) => `translate(${d.x ?? 0},${d.y ?? 0})`);
      });

      // Pan/zoom.
      const zoomBehavior = zoom
        .zoom<SVGSVGElement, unknown>()
        .scaleExtent([0.3, 3])
        .on("zoom", (event) => {
          root.attr("transform", event.transform);
        });
      d3sel.select(svg).call(zoomBehavior);

      return () => {
        simulation.stop();
        d3sel.select(svg).on(".zoom", null);
      };
    });
  }, [data]);

  if (data.nodes.length === 0) {
    return (
      <div className="flex items-center justify-center h-48 text-muted text-sm">
        No symbol data — run <code className="text-xs mx-1">crabcc index</code> first.
      </div>
    );
  }

  return (
    <div className="relative rounded border border-border bg-background overflow-hidden">
      {!d3Ready && (
        <div className="absolute inset-0 flex items-center justify-center bg-background/80 z-20 text-muted text-sm">
          <span className="animate-spin mr-2">⟳</span> Loading graph…
        </div>
      )}
      <div className="absolute top-2 right-2 flex gap-3 text-[10px] text-muted z-10">
        <span className="flex items-center gap-1">
          <span
            className="inline-block w-2.5 h-2.5 rounded-full"
            style={{ background: "var(--color-primary)" }}
          />
          changed ({data.direct_symbols})
        </span>
        <span className="flex items-center gap-1">
          <span
            className="inline-block w-2 h-2 rounded-full"
            style={{ background: "var(--color-destructive)" }}
          />
          impacted ({data.impacted_symbols})
        </span>
      </div>
      <svg
        ref={svgRef}
        className="w-full"
        style={{ height: "360px" }}
        aria-label="PR impact graph"
      />
    </div>
  );
});
