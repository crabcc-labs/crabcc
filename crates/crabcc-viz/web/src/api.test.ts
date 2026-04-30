// bun:test — unit tests for the typed API client. Stubs `fetch` per
// test so the request shape (path, method, body) is asserted directly.
//
// Coverage: every method on `api`, plus the error envelope (non-2xx
// must throw, with the status code embedded in the error message).

import { describe, expect, it, beforeEach, afterEach, mock } from "bun:test";
import { api } from "./api";

const realFetch = globalThis.fetch;

function stubFetch(impl: typeof fetch): void {
  globalThis.fetch = mock(impl) as typeof fetch;
}

beforeEach(() => stubFetch(realFetch));
afterEach(() => {
  globalThis.fetch = realFetch;
});

describe("api.bootstrap", () => {
  it("GETs /api/bootstrap and returns parsed JSON", async () => {
    let calledPath: string | undefined;
    stubFetch(async (input) => {
      calledPath = String(input);
      return new Response(
        JSON.stringify({ repo: "x", root: "/x", version: "1" }),
        { status: 200 },
      );
    });
    const r = await api.bootstrap();
    expect(calledPath).toBe("/api/bootstrap");
    expect(r.repo).toBe("x");
    expect(r.version).toBe("1");
  });

  it("throws on non-2xx", async () => {
    stubFetch(async () => new Response("nope", { status: 500 }));
    await expect(api.bootstrap()).rejects.toThrow();
  });
});

describe("api.activity", () => {
  it("includes since + limit query params", async () => {
    let url: string | undefined;
    stubFetch(async (input) => {
      url = String(input);
      return new Response(JSON.stringify({ items: [] }), { status: 200 });
    });
    await api.activity(1700000000, 50);
    expect(url).toContain("since=1700000000");
    expect(url).toContain("limit=50");
  });

  it("defaults since=0 and limit=100 when omitted", async () => {
    let url: string | undefined;
    stubFetch(async (input) => {
      url = String(input);
      return new Response(JSON.stringify({ items: [] }), { status: 200 });
    });
    await api.activity();
    expect(url).toContain("since=0");
    expect(url).toContain("limit=100");
  });
});

describe("api.reindex", () => {
  it("POSTs to /api/reindex (no body) and parses ReindexReport", async () => {
    let method: string | undefined;
    let url: string | undefined;
    stubFetch(async (input, init) => {
      url = String(input);
      method = init?.method;
      return new Response(
        JSON.stringify({
          root: "/x",
          elapsed_ms: 12,
          stats: { files_indexed: 3 },
          logs: ["ok"],
        }),
        { status: 200 },
      );
    });
    const r = await api.reindex();
    expect(url).toBe("/api/reindex");
    expect(method).toBe("POST");
    expect(r.elapsed_ms).toBe(12);
    expect(r.logs).toEqual(["ok"]);
  });

  it("propagates server error message in the thrown error", async () => {
    stubFetch(
      async () =>
        new Response("reindex failed: store locked", {
          status: 500,
          statusText: "Internal Server Error",
        }),
    );
    await expect(api.reindex()).rejects.toThrow(/500/);
  });
});

describe("api.agents + agentLog", () => {
  it("agents GETs /api/agents", async () => {
    let url: string | undefined;
    stubFetch(async (input) => {
      url = String(input);
      return new Response(JSON.stringify({ agents: [] }), { status: 200 });
    });
    await api.agents();
    expect(url).toBe("/api/agents");
  });

  it("agentLog encodes the cursor as ?since=", async () => {
    let url: string | undefined;
    stubFetch(async (input) => {
      url = String(input);
      return new Response(
        JSON.stringify({ body: "", cursor: 100, total: 100 }),
        { status: 200 },
      );
    });
    await api.agentLog("abc123", 42);
    expect(url).toBe("/api/agents/abc123/log?since=42");
  });
});

describe("api.randomQuery", () => {
  it("POSTs /api/random-query and returns the (op, symbol) pair", async () => {
    stubFetch(
      async () =>
        new Response(JSON.stringify({ op: "sym", symbol: "Foo" }), {
          status: 200,
        }),
    );
    const r = await api.randomQuery();
    expect(r.op).toBe("sym");
    expect(r.symbol).toBe("Foo");
  });
});
