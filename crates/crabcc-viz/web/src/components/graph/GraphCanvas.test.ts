// Pure tests for the canvas zoom math. The renderer itself needs a
// DOM and is exercised by the in-browser screenshot verification step.

import { describe, expect, it } from "bun:test";
import { __testables } from "./GraphCanvas";

const { zoomAround, clamp } = __testables;

describe("zoomAround", () => {
  it("keeps the focal point fixed in screen space when zooming in", () => {
    const t = { k: 1, x: 0, y: 0 };
    const cx = 200;
    const cy = 100;
    // World point under (cx, cy) before zoom: ((200-0)/1, (100-0)/1) = (200,100).
    const t2 = zoomAround(t, 2, cx, cy);
    // Screen-space coord of that same world point after the new transform.
    const sx = t2.x + 200 * t2.k;
    const sy = t2.y + 100 * t2.k;
    expect(Math.abs(sx - cx)).toBeLessThan(1e-9);
    expect(Math.abs(sy - cy)).toBeLessThan(1e-9);
    expect(t2.k).toBe(2);
  });

  it("clamps scale to the [0.15, 8] window", () => {
    const t = { k: 1, x: 0, y: 0 };
    expect(zoomAround(t, 100, 0, 0).k).toBe(8);
    expect(zoomAround(t, 1 / 100, 0, 0).k).toBe(0.15);
  });
});

describe("clamp", () => {
  it("returns the value untouched when in range", () => {
    expect(clamp(0.5, 0, 1)).toBe(0.5);
  });
  it("snaps below-min and above-max to the bounds", () => {
    expect(clamp(-1, 0, 1)).toBe(0);
    expect(clamp(2, 0, 1)).toBe(1);
  });
});
