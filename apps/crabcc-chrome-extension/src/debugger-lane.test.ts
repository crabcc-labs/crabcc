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
});
