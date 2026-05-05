// Smoke tests for the tool-family icon set. Mirrors the Rust-side
// unit tests in `crabcc-desktop/src/icons.rs`: every icon produces
// output, declares a 16×16 viewBox, and uses `currentColor` (not
// hardcoded theme hexes) so consumers can theme-tint without a
// per-palette fork.
//
// We render each component to its underlying React-element tree
// (no DOM) and walk it for assertions; this avoids a JSDOM dep
// and keeps the test ~10 ms.

import { describe, expect, test } from "bun:test";
import { renderToString } from "react-dom/server";
import { TOOL_ICONS, type ToolName } from "./tool-icons";

const NAMES: ToolName[] = [
  "sym",
  "refs",
  "callers",
  "outline",
  "fuzzy",
  "memory",
  "fetch",
  "agent",
  "index",
  "serve",
  "mcp",
];

describe("tool-icons", () => {
  test("TOOL_ICONS lookup map covers every tool family", () => {
    for (const name of NAMES) {
      expect(TOOL_ICONS[name]).toBeDefined();
    }
    // Length-pinning so a future variant added to ToolName without
    // a TOOL_ICONS entry trips this test rather than rendering a
    // silent `undefined` component at runtime.
    expect(Object.keys(TOOL_ICONS).length).toBe(11);
  });

  test.each(NAMES)("%s: renders a 16x16 currentColor SVG", (name) => {
    const Icon = TOOL_ICONS[name];
    const html = renderToString(<Icon />);
    expect(html).toContain("<svg");
    expect(html).toContain('viewBox="0 0 16 16"');
    expect(html).toContain("currentColor");
    // Hardcoded theme hexes would freeze the icon's hue across
    // palette switches — exactly the bug currentColor is meant to
    // prevent.
    expect(html).not.toContain("#E6E6EB");
    expect(html).not.toContain("#0E0E12");
  });

  test("size prop overrides the default 16px edge", () => {
    const Icon = TOOL_ICONS.sym;
    const html = renderToString(<Icon size={32} />);
    expect(html).toContain('width="32"');
    expect(html).toContain('height="32"');
    // viewBox stays 16×16 so the strokes scale, not get cropped.
    expect(html).toContain('viewBox="0 0 16 16"');
  });

  test("custom className passes through to the root <svg>", () => {
    const Icon = TOOL_ICONS.refs;
    const html = renderToString(<Icon className="text-primary opacity-70" />);
    expect(html).toContain('class="text-primary opacity-70"');
  });
});
