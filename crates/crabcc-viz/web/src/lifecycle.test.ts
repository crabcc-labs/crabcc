// bun:test — verifies the once-per-state-transition contract for
// `lifecycle.ts`. We capture every `console.info` line into an array
// so we can assert exactly which lines fire (and which stay silent)
// under repeated polls, error spells, and recoveries.
//
// Coverage:
//   - mount/unmount each emit one line
//   - first fetchOk emits a summary; subsequent equal summaries stay silent
//   - shape change emits an `old → new` line
//   - error spell logs once; repeat errors stay silent
//   - recovery (next successful fetch) logs once
//   - silenced via localStorage.crabcc_silent="1"
//   - SSE connect/disconnect dedupe across rapid bursts

import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import {
  __resetLifecycleStateForTests,
  logFetchErr,
  logFetchOk,
  logMount,
  logSseConnect,
  logSseDisconnect,
  logUnmount,
  logUserAction,
} from "./lifecycle";

let captured: string[] = [];
let realInfo: typeof console.info;

beforeEach(() => {
  __resetLifecycleStateForTests();
  captured = [];
  realInfo = console.info;
  console.info = ((msg: unknown) => {
    captured.push(typeof msg === "string" ? msg : String(msg));
  }) as typeof console.info;
  try {
    globalThis.localStorage?.removeItem("crabcc_silent");
  } catch {
    // No-op when localStorage isn't shimmed in this run.
  }
});

afterEach(() => {
  console.info = realInfo;
});

describe("mount/unmount", () => {
  it("each emits exactly one info line", () => {
    logMount("Foo");
    logUnmount("Foo");
    expect(captured).toHaveLength(2);
    expect(captured[0]).toContain("Foo mounted");
    expect(captured[1]).toContain("Foo unmounted");
  });
});

describe("logFetchOk", () => {
  it("logs the first summary", () => {
    logFetchOk("/api/x", "1 row");
    expect(captured).toHaveLength(1);
    expect(captured[0]).toContain("/api/x: 1 row");
  });

  it("stays silent when the summary is unchanged", () => {
    logFetchOk("/api/x", "1 row");
    logFetchOk("/api/x", "1 row");
    logFetchOk("/api/x", "1 row");
    expect(captured).toHaveLength(1);
  });

  it("emits old → new when the summary changes", () => {
    logFetchOk("/api/x", "1 row");
    logFetchOk("/api/x", "2 rows");
    expect(captured).toHaveLength(2);
    expect(captured[1]).toContain("1 row → 2 rows");
  });
});

describe("error spell + recovery", () => {
  it("logs once on first error, stays silent on retries", () => {
    logFetchErr("/api/x", new Error("boom"));
    logFetchErr("/api/x", new Error("boom"));
    logFetchErr("/api/x", new Error("boom"));
    expect(captured).toHaveLength(1);
    expect(captured[0]).toContain("failed: boom");
  });

  it("logs recovery once on next successful fetch", () => {
    logFetchErr("/api/x", new Error("boom"));
    logFetchErr("/api/x", new Error("boom"));
    logFetchOk("/api/x", "1 row");
    expect(captured).toHaveLength(3);
    expect(captured[1]).toContain("/api/x recovered after 2 failures");
    expect(captured[2]).toContain("/api/x: 1 row");
  });
});

describe("SSE connect/disconnect", () => {
  it("dedupes repeated connects and disconnects per path", () => {
    logSseConnect("/api/events");
    logSseConnect("/api/events");
    logSseDisconnect("/api/events");
    logSseDisconnect("/api/events");
    logSseConnect("/api/events");
    expect(captured).toHaveLength(3);
    expect(captured[0]).toContain("SSE connected");
    expect(captured[1]).toContain("SSE disconnected");
    expect(captured[2]).toContain("SSE connected");
  });
});

describe("logUserAction", () => {
  it("emits one info line per call", () => {
    logUserAction("reindex requested");
    expect(captured).toHaveLength(1);
    expect(captured[0]).toContain("reindex requested");
  });
});

describe("crabcc_silent localStorage gate", () => {
  it("suppresses all output when set", () => {
    if (typeof globalThis.localStorage === "undefined") {
      // Bun runs without DOM by default — skip if no localStorage shim.
      return;
    }
    globalThis.localStorage.setItem("crabcc_silent", "1");
    logMount("Foo");
    logFetchOk("/api/x", "anything");
    logFetchErr("/api/x", new Error("boom"));
    logSseConnect("/api/events");
    expect(captured).toHaveLength(0);
    globalThis.localStorage.removeItem("crabcc_silent");
  });
});
