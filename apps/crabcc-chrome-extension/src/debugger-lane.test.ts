import { describe, it, expect, beforeEach } from "bun:test";
import { fireDebuggerEvent, fireTabRemoved, getShim, resetShim } from "./__test_shim";
import * as dbg from "./debugger-lane";

describe("debugger-lane", () => {
  beforeEach(async () => {
    // The lane keeps per-tab buffers in module-level Maps that resetShim
    // doesn't reach. Detach every tab we may have touched so each test
    // starts with empty buffers.
    for (const tabId of [1, 2]) {
      if (dbg.isAttached(tabId)) await dbg.detach(tabId);
    }
    resetShim();
  });

  it("methods refuse to run before attach", async () => {
    let caught: Error | null = null;
    try {
      await dbg.evaluate(1, "1+1");
    } catch (err) {
      caught = err as Error;
    }
    expect(caught?.message).toMatch(/not attached/);
    expect(dbg.consoleList(1)).toEqual([]);
    expect(dbg.networkList(1)).toEqual([]);
  });

  it("attach enables Runtime + Network domains", async () => {
    await dbg.attach(1);
    const cmds = getShim().debugger.__commands.map((c) => c.method);
    expect(cmds).toContain("Runtime.enable");
    expect(cmds).toContain("Network.enable");
    expect(dbg.isAttached(1)).toBe(true);
  });

  it("Runtime.consoleAPICalled events land in the buffer", async () => {
    await dbg.attach(1);
    fireDebuggerEvent(1, "Runtime.consoleAPICalled", {
      type: "warn",
      args: [{ type: "string", value: "hello" }],
      timestamp: 12345,
      stackTrace: { callFrames: [{ url: "https://x/y.js", lineNumber: 7, columnNumber: 3 }] },
    });
    const list = dbg.consoleList(1);
    expect(list).toHaveLength(1);
    expect(list[0].level).toBe("warn");
    expect(list[0].text).toBe("hello");
    expect(list[0].source).toBe("https://x/y.js");
    expect(list[0].line).toBe(7);
  });

  it("Network request lifecycle merges into a single entry", async () => {
    await dbg.attach(1);
    fireDebuggerEvent(1, "Network.requestWillBeSent", {
      requestId: "r1",
      request: { url: "https://x/data", method: "POST" },
      type: "XHR",
    });
    fireDebuggerEvent(1, "Network.responseReceived", {
      requestId: "r1",
      response: { status: 200, statusText: "OK", mimeType: "application/json" },
    });
    fireDebuggerEvent(1, "Network.loadingFinished", {
      requestId: "r1",
      encodedDataLength: 4096,
    });
    const list = dbg.networkList(1);
    expect(list).toHaveLength(1);
    expect(list[0].method).toBe("POST");
    expect(list[0].status).toBe(200);
    expect(list[0].mimeType).toBe("application/json");
    expect(list[0].size).toBe(4096);
    expect(list[0].failed).toBe(false);
  });

  it("Network.loadingFailed flips the failed flag and records errorText", async () => {
    await dbg.attach(1);
    fireDebuggerEvent(1, "Network.requestWillBeSent", {
      requestId: "r2",
      request: { url: "https://nope/" },
      type: "Document",
    });
    fireDebuggerEvent(1, "Network.loadingFailed", {
      requestId: "r2",
      errorText: "net::ERR_NAME_NOT_RESOLVED",
    });
    const list = dbg.networkList(1);
    expect(list[0].failed).toBe(true);
    expect(list[0].errorText).toMatch(/ERR_NAME_NOT_RESOLVED/);
  });

  it("evaluate returns the Runtime.evaluate result", async () => {
    await dbg.attach(1);
    getShim().debugger.__nextCommand["Runtime.evaluate"] = {
      result: { type: "string", value: "Example" },
    };
    const res = await dbg.evaluate(1, "document.title");
    expect(res.value).toBe("Example");
    expect(res.exception).toBeNull();
  });

  it("evaluate surfaces exceptionDetails as an exception result", async () => {
    await dbg.attach(1);
    getShim().debugger.__nextCommand["Runtime.evaluate"] = {
      exceptionDetails: { text: "Uncaught", exception: { type: "object", description: "ReferenceError: x is not defined" } },
    };
    const res = await dbg.evaluate(1, "x");
    expect(res.exception).toMatch(/ReferenceError/);
    expect(res.value).toBeNull();
    expect(res.type).toBe("exception");
  });

  it("tab close auto-detaches and clears buffers", async () => {
    await dbg.attach(1);
    fireDebuggerEvent(1, "Runtime.consoleAPICalled", {
      type: "log",
      args: [{ type: "string", value: "x" }],
    });
    expect(dbg.consoleList(1)).toHaveLength(1);
    fireTabRemoved(1);
    expect(dbg.isAttached(1)).toBe(false);
    expect(dbg.consoleList(1)).toEqual([]);
  });

  it("detach clears buffers", async () => {
    await dbg.attach(2);
    fireDebuggerEvent(2, "Runtime.consoleAPICalled", {
      type: "log",
      args: [{ type: "string", value: "y" }],
    });
    expect(dbg.consoleList(2)).toHaveLength(1);
    await dbg.detach(2);
    expect(dbg.consoleList(2)).toEqual([]);
    expect(dbg.isAttached(2)).toBe(false);
  });

  // --- v8 lane ----------------------------------------------------------

  it("v8.collectGarbage enables HeapProfiler then runs the gc command", async () => {
    await dbg.attach(1);
    await dbg.v8CollectGarbage(1);
    const cmds = getShim().debugger.__commands.map((c) => c.method);
    expect(cmds).toContain("HeapProfiler.enable");
    expect(cmds).toContain("HeapProfiler.collectGarbage");
  });

  it("v8.collectGarbage skips re-enabling on the second call", async () => {
    await dbg.attach(1);
    await dbg.v8CollectGarbage(1);
    getShim().debugger.__commands.length = 0;
    await dbg.v8CollectGarbage(1);
    const cmds = getShim().debugger.__commands.map((c) => c.method);
    // HeapProfiler.enable should NOT appear the second time — already cached.
    expect(cmds).toEqual(["HeapProfiler.collectGarbage"]);
  });

  it("v8.heapSnapshot streams chunks via the debugger event channel", async () => {
    await dbg.attach(1);
    // sendCommand for HeapProfiler.takeHeapSnapshot returns a resolved
    // promise — fire chunk events synchronously *before* the await
    // resolves by stubbing sendCommand to fire then resolve.
    const shim = getShim();
    const orig = shim.debugger.sendCommand;
    shim.debugger.sendCommand = async (target, method, params) => {
      if (method === "HeapProfiler.takeHeapSnapshot") {
        fireDebuggerEvent(target.tabId, "HeapProfiler.addHeapSnapshotChunk", { chunk: '{"a":' });
        fireDebuggerEvent(target.tabId, "HeapProfiler.addHeapSnapshotChunk", { chunk: "1}" });
      }
      return orig(target, method, params);
    };
    const res = await dbg.v8HeapSnapshot(1);
    expect(res.json).toBe('{"a":1}');
    expect(res.chunkCount).toBe(2);
    expect(res.sizeBytes).toBe(7);
  });

  it("v8.profile.start then stop returns a profile summary", async () => {
    await dbg.attach(1);
    getShim().debugger.__nextCommand["Profiler.stop"] = {
      profile: {
        nodes: [{ id: 1 }, { id: 2 }, { id: 3 }],
        samples: [1, 2, 1, 3],
        // CDP timestamps are in microseconds — 250000 us = 250 ms.
        startTime: 1000,
        endTime: 251000,
      },
    };
    await dbg.v8ProfileStart(1);
    const sum = await dbg.v8ProfileStop(1);
    expect(sum.nodeCount).toBe(3);
    expect(sum.sampleCount).toBe(4);
    expect(sum.durationMs).toBe(250);
  });

  it("v8.profile.start refuses a second start without stop", async () => {
    await dbg.attach(1);
    await dbg.v8ProfileStart(1);
    let caught: Error | null = null;
    try {
      await dbg.v8ProfileStart(1);
    } catch (err) {
      caught = err as Error;
    }
    expect(caught?.message).toMatch(/already running/);
  });

  it("v8.profile.stop without start fails", async () => {
    await dbg.attach(1);
    let caught: Error | null = null;
    try {
      await dbg.v8ProfileStop(1);
    } catch (err) {
      caught = err as Error;
    }
    expect(caught?.message).toMatch(/no profile running/);
  });

  it("v8.metrics returns the Performance.getMetrics payload", async () => {
    await dbg.attach(1);
    getShim().debugger.__nextCommand["Performance.getMetrics"] = {
      metrics: [
        { name: "JSHeapUsedSize", value: 1024 * 1024 * 32 },
        { name: "Documents", value: 1 },
        { name: "Nodes", value: 247 },
      ],
    };
    const out = await dbg.v8Metrics(1);
    expect(out.metrics).toHaveLength(3);
    expect(out.metrics[0]).toEqual({ name: "JSHeapUsedSize", value: 33554432 });
    expect(typeof out.ts).toBe("number");
  });
});
