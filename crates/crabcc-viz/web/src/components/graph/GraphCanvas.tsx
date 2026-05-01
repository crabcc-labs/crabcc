// Canvas-based graph renderer. SVG with hundreds of <line>/<g>/<circle>
// nodes hits a re-paint wall during pan/zoom (every node element gets
// a transform-attribute update each frame), so we drop to a single
// <canvas> and paint the whole scene in one pass per frame.
//
// Pan/zoom is handled with a simple affine transform we apply to the
// canvas 2D context. A non-passive wheel listener is installed via
// ref (React's onWheel is registered passively, so e.preventDefault()
// throws). Hit-testing is O(N) — fine up to a few thousand nodes.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Layout, SimNode } from "./types";

const DEPTH_COLORS = [
  "#ff2a6d", // depth 0 / seed
  "#00f0ff",
  "#7a5cff",
  "#ffd166",
  "#9bff8f",
  "#ff8f4a",
  "#cccccc",
];
const NODE_R_SEED = 7;
const NODE_R_DEFAULT = 4.5;
const HIT_SLOP = 4; // extra px around node center for forgiving click hit-test.

export interface Transform {
  k: number; // scale
  x: number; // translate-x
  y: number;
}

const IDENTITY: Transform = { k: 1, x: 0, y: 0 };

interface Props {
  layout: Layout;
  width: number;
  height: number;
  pinned: SimNode | null;
  highlight: Set<string> | null;
  onPick: (node: SimNode | null) => void;
  onHover: (node: SimNode | null) => void;
  /** Imperative handle to drive zoom/pan from keyboard or buttons. */
  controlRef?: React.MutableRefObject<CanvasController | null>;
}

export interface CanvasController {
  reset(): void;
  pan(dx: number, dy: number): void;
  zoom(factor: number, cx?: number, cy?: number): void;
  centerOn(node: SimNode): void;
  getTransform(): Transform;
}

export function GraphCanvas({
  layout,
  width,
  height,
  pinned,
  highlight,
  onPick,
  onHover,
  controlRef,
}: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [transform, setTransform] = useState<Transform>(IDENTITY);
  const transformRef = useRef(transform);
  transformRef.current = transform;

  // Drag state — held in a ref so we don't re-render every mousemove.
  const dragRef = useRef<{ x: number; y: number; t0: Transform } | null>(null);

  // Center (0,0) of the simulation onto the middle of the viewport;
  // every coordinate transform afterward composes on top of that.
  const baseOffset = useMemo(
    () => ({ x: width / 2, y: height / 2 }),
    [width, height],
  );

  // ── Imperative controller — exposed to parent for keyboard shortcuts.
  useEffect(() => {
    if (!controlRef) return;
    const ctl: CanvasController = {
      reset: () => setTransform(IDENTITY),
      pan: (dx, dy) =>
        setTransform((t) => ({ ...t, x: t.x + dx, y: t.y + dy })),
      zoom: (factor, cx = width / 2, cy = height / 2) => {
        setTransform((t) => zoomAround(t, factor, cx, cy));
      },
      centerOn: (n) => {
        const k = Math.max(transformRef.current.k, 1.4);
        setTransform({
          k,
          x: width / 2 - (n.x ?? 0) * k - baseOffset.x * k,
          y: height / 2 - (n.y ?? 0) * k - baseOffset.y * k,
        });
      },
      getTransform: () => transformRef.current,
    };
    controlRef.current = ctl;
    return () => {
      if (controlRef.current === ctl) controlRef.current = null;
    };
  }, [controlRef, width, height, baseOffset]);

  // ── Wheel zoom — non-passive so we can preventDefault.
  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const cx = e.clientX - rect.left;
      const cy = e.clientY - rect.top;
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      setTransform((t) => zoomAround(t, factor, cx, cy));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  // ── Paint. Re-runs on layout, transform, hover/pin/highlight, viewport.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(width * dpr);
    canvas.height = Math.floor(height * dpr);
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    // Background.
    ctx.fillStyle = "#161618";
    ctx.fillRect(0, 0, width, height);

    // Compose the world transform: viewport-center offset, then user pan/zoom.
    ctx.save();
    ctx.translate(transform.x, transform.y);
    ctx.scale(transform.k, transform.k);
    ctx.translate(baseOffset.x, baseOffset.y);

    // Links — drawn first so nodes paint on top.
    const pinnedId = pinned?.id ?? null;
    const haveHighlight = highlight !== null;
    ctx.lineWidth = 1 / transform.k;
    for (const l of layout.links) {
      const sx = l.source.x ?? 0;
      const sy = l.source.y ?? 0;
      const tx = l.target.x ?? 0;
      const ty = l.target.y ?? 0;

      const onPin =
        pinnedId !== null && (l.source.id === pinnedId || l.target.id === pinnedId);
      const dim =
        haveHighlight &&
        !(highlight!.has(l.source.id) && highlight!.has(l.target.id));

      ctx.strokeStyle = onPin
        ? "rgba(255,42,109,0.65)"
        : dim
          ? "rgba(200,212,255,0.06)"
          : "rgba(200,212,255,0.22)";
      ctx.lineWidth = (onPin ? 1.4 : 1) / transform.k;
      ctx.beginPath();
      ctx.moveTo(sx, sy);
      ctx.lineTo(tx, ty);
      ctx.stroke();
    }

    // Nodes.
    for (const n of layout.nodes) {
      const x = n.x ?? 0;
      const y = n.y ?? 0;
      const r = n.isSeed ? NODE_R_SEED : NODE_R_DEFAULT;
      const dim = haveHighlight && !highlight!.has(n.id);
      ctx.globalAlpha = dim ? 0.18 : 0.95;
      ctx.fillStyle = colorFor(n);
      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.lineWidth = (n.id === pinnedId ? 2 : n.isSeed ? 1.2 : 0.6) / transform.k;
      ctx.strokeStyle =
        n.id === pinnedId ? "#ff2a6d" : n.isSeed ? "#fff" : "rgba(255,255,255,0.25)";
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    ctx.restore();
  }, [layout, transform, width, height, pinned, highlight, baseOffset]);

  // ── Hit-testing. Convert client coords back through the world transform.
  const pickAt = useCallback(
    (px: number, py: number): SimNode | null => {
      const t = transformRef.current;
      const wx = (px - t.x) / t.k - baseOffset.x;
      const wy = (py - t.y) / t.k - baseOffset.y;
      let best: SimNode | null = null;
      let bestD2 = Infinity;
      for (const n of layout.nodes) {
        const dx = (n.x ?? 0) - wx;
        const dy = (n.y ?? 0) - wy;
        const d2 = dx * dx + dy * dy;
        const r = (n.isSeed ? NODE_R_SEED : NODE_R_DEFAULT) + HIT_SLOP / t.k;
        if (d2 <= r * r && d2 < bestD2) {
          best = n;
          bestD2 = d2;
        }
      }
      return best;
    },
    [layout, baseOffset],
  );

  const onMouseDown = useCallback((e: React.MouseEvent) => {
    dragRef.current = {
      x: e.clientX,
      y: e.clientY,
      t0: { ...transformRef.current },
    };
  }, []);
  const onMouseMove = useCallback(
    (e: React.MouseEvent) => {
      const drag = dragRef.current;
      if (drag) {
        const dx = e.clientX - drag.x;
        const dy = e.clientY - drag.y;
        setTransform({ ...drag.t0, x: drag.t0.x + dx, y: drag.t0.y + dy });
        return;
      }
      const rect = canvasRef.current?.getBoundingClientRect();
      if (!rect) return;
      const node = pickAt(e.clientX - rect.left, e.clientY - rect.top);
      onHover(node);
    },
    [onHover, pickAt],
  );
  const onMouseUp = useCallback(
    (e: React.MouseEvent) => {
      const drag = dragRef.current;
      dragRef.current = null;
      if (!drag) return;
      const moved = Math.hypot(e.clientX - drag.x, e.clientY - drag.y);
      // Treat sub-pixel movement as a click, not a drag.
      if (moved < 3) {
        const rect = canvasRef.current?.getBoundingClientRect();
        if (!rect) return;
        const node = pickAt(e.clientX - rect.left, e.clientY - rect.top);
        onPick(node);
      }
    },
    [onPick, pickAt],
  );
  const onMouseLeave = useCallback(() => {
    dragRef.current = null;
    onHover(null);
  }, [onHover]);

  const onDoubleClick = useCallback(() => setTransform(IDENTITY), []);

  return (
    <canvas
      ref={canvasRef}
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onMouseLeave={onMouseLeave}
      onDoubleClick={onDoubleClick}
      style={{
        display: "block",
        cursor: dragRef.current ? "grabbing" : "grab",
        touchAction: "none",
        width: "100%",
        height: "100%",
      }}
    />
  );
}

function zoomAround(t: Transform, factor: number, cx: number, cy: number): Transform {
  const k2 = clamp(t.k * factor, 0.15, 8);
  // Keep the world point under (cx,cy) fixed.
  const f = k2 / t.k;
  return {
    k: k2,
    x: cx - (cx - t.x) * f,
    y: cy - (cy - t.y) * f,
  };
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

export function colorFor(node: { isSeed: boolean; depth: number }): string {
  if (node.isSeed) return DEPTH_COLORS[0];
  return DEPTH_COLORS[Math.min(node.depth, DEPTH_COLORS.length - 1)];
}

export const __testables = { zoomAround, clamp };
