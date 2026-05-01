import { describe, it, expect, beforeEach } from "bun:test";
import { getShim, resetShim } from "./__test_shim";
import { callBridge, dispatchRpc } from "./bridge-rpc";

describe("bridge-rpc", () => {
  beforeEach(() => {
    resetShim();
  });

  it("callBridge returns the injected function's result", async () => {
    getShim().scripting.__next = { result: { hello: "world" } };
    const out = await callBridge<{ hello: string }>(1, "ariaSnapshot", []);
    expect(out).toEqual({ hello: "world" });
  });

  it("callBridge surfaces per-frame errors thrown inside the page", async () => {
    getShim().scripting.__next = { error: { message: "window.__crabcc__ missing" } };
    let caught: Error | null = null;
    try {
      await callBridge(1, "click", ["#nope"]);
    } catch (err) {
      caught = err as Error;
    }
    expect(caught?.message).toBe("window.__crabcc__ missing");
  });

  it("callBridge throws when executeScript returns no frames", async () => {
    // Simulate a tab that's been closed mid-call.
    getShim().scripting.executeScript = async () => [];
    let caught: Error | null = null;
    try {
      await callBridge(1, "click", ["#x"]);
    } catch (err) {
      caught = err as Error;
    }
    expect(caught?.message).toMatch(/no frames/);
  });

  it("dispatchRpc returns an ok envelope on success", async () => {
    getShim().scripting.__next = { result: 42 };
    const res = await dispatchRpc(1, { id: 7, method: "schema", args: [] });
    expect(res).toEqual({ id: 7, ok: true, result: 42 });
  });

  it("dispatchRpc returns an error envelope on throw", async () => {
    getShim().scripting.__next = { throws: new Error("boom") };
    const res = await dispatchRpc(1, { id: 8, method: "click", args: ["#x"] });
    expect(res).toEqual({ id: 8, ok: false, error: "boom" });
  });
});
