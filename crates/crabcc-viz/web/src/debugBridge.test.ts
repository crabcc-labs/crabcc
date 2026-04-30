import { describe, it, expect, beforeEach } from "bun:test";
import { installDebugBridge, updateDebugBridge } from "./debugBridge";

declare const window: { __crabcc__?: unknown };

describe("debugBridge", () => {
  beforeEach(() => {
    const g = globalThis as unknown as { window?: { __crabcc__?: unknown } };
    if (g.window) {
      g.window.__crabcc__ = undefined;
    }
  });

  it("installs window.__crabcc__ with schema v1", () => {
    const win: { __crabcc__?: unknown } = {};
    (globalThis as unknown as { window: typeof win }).window = win;
    const bridge = installDebugBridge();
    expect(bridge.schemaVersion).toBe(1);
    expect(win.__crabcc__).toBe(bridge as unknown);
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
