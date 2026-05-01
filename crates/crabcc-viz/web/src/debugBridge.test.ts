import { describe, it, expect, beforeEach } from "bun:test";
import {
  installDebugBridge,
  snapshotInteractives,
  updateDebugBridge,
} from "./debugBridge";

declare const window: { __crabcc__?: unknown };

describe("debugBridge", () => {
  beforeEach(() => {
    const g = globalThis as unknown as { window?: { __crabcc__?: unknown } };
    if (g.window) {
      g.window.__crabcc__ = undefined;
    }
  });

  it("installs window.__crabcc__ with schema v2", () => {
    const win: { __crabcc__?: unknown } = {};
    (globalThis as unknown as { window: typeof win }).window = win;
    const bridge = installDebugBridge();
    expect(bridge.schemaVersion).toBe(2);
    expect(win.__crabcc__).toBe(bridge as unknown);
  });

  it("exposes browser-automation primitives on the bridge", () => {
    const win: { __crabcc__?: unknown } = {};
    (globalThis as unknown as { window: typeof win }).window = win;
    const bridge = installDebugBridge();
    for (const name of [
      "navigate",
      "goBack",
      "goForward",
      "pressKey",
      "hover",
      "type",
      "selectOption",
      "drag",
      "ariaSnapshot",
      "clickByRef",
      "hoverByRef",
      "typeByRef",
    ] as const) {
      expect(typeof (bridge as unknown as Record<string, unknown>)[name]).toBe(
        "function",
      );
    }
  });

  it("ariaSnapshot returns an empty document when DOM is absent", () => {
    const orig = (globalThis as { document?: unknown }).document;
    delete (globalThis as { document?: unknown }).document;
    const win: { __crabcc__?: unknown } = {};
    (globalThis as unknown as { window: typeof win }).window = win;
    const bridge = installDebugBridge();
    const snap = bridge.ariaSnapshot();
    expect(snap.role).toBe("document");
    expect(snap.children).toEqual([]);
    if (orig !== undefined) {
      (globalThis as { document?: unknown }).document = orig;
    }
  });

  it("snapshotInteractives returns [] without DOM", () => {
    // bun test runs without a DOM by default; the bridge falls back to []
    // in non-document contexts so callers don't crash.
    const orig = (globalThis as { document?: unknown }).document;
    delete (globalThis as { document?: unknown }).document;
    expect(snapshotInteractives()).toEqual([]);
    if (orig !== undefined) {
      (globalThis as { document?: unknown }).document = orig;
    }
  });

  it("notifies subscribers when state updates", () => {
    const win: { __crabcc__?: unknown } = {};
    (globalThis as unknown as { window: typeof win }).window = win;
    const bridge = installDebugBridge();
    let calls = 0;
    let lastCount = -1;
    const off = bridge.subscribe((s) => {
      calls += 1;
      lastCount = s.agentCount;
    });
    expect(calls).toBe(1); // initial fire on subscribe
    updateDebugBridge({ agentCount: 7 });
    expect(calls).toBe(2);
    expect(lastCount).toBe(7);
    off();
    updateDebugBridge({ agentCount: 99 });
    expect(calls).toBe(2); // unsubscribed
  });
});
