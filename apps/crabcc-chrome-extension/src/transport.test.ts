import { describe, it, expect, beforeEach } from "bun:test";
import { getLastWs, getShim, resetShim } from "./__test_shim";
import * as transport from "./transport";
import type { RpcRequest } from "./bridge-types";

// Helper: wait one queued microtask so the FakeWS can flip into the OPEN
// state and fire `onopen`. Matches the production code's expectation that
// connect() resolves asynchronously.
function nextTick(ms = 5): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

describe("transport", () => {
  beforeEach(() => {
    resetShim();
    // Reset module-level state in transport.ts by disconnecting.
    transport.disconnect();
  });

  it("bootstrap with no stored auto-flag leaves the connection closed", async () => {
    await transport.bootstrap();
    const snap = transport.getSnapshot();
    expect(snap.state).toBe("disconnected");
    // bootstrap should still persist the endpoint so the popup picks it up.
    expect(snap.endpoint).toMatch(/^ws:\/\//);
  });

  it("connect → fires hello on socket open", async () => {
    transport.connect("ws://test.local/");
    await nextTick();
    const ws = getLastWs();
    expect(ws).not.toBeNull();
    expect(transport.getSnapshot().state).toBe("connected");
    const sent = ws!.__sent.map((s) => JSON.parse(s));
    expect(sent[0].kind).toBe("hello");
    expect(sent[0].schema).toBe(2);
    expect(Array.isArray(sent[0].capabilities)).toBe(true);
  });

  it("ping from server is echoed as pong", async () => {
    transport.connect("ws://test.local/");
    await nextTick();
    const ws = getLastWs()!;
    ws.__sent.length = 0;
    ws.__recv(JSON.stringify({ kind: "ping", ts: 1234 }));
    expect(ws.__sent).toHaveLength(1);
    const pong = JSON.parse(ws.__sent[0]);
    expect(pong).toEqual({ kind: "pong", ts: 1234 });
  });

  it("RpcRequest from server is dispatched through setHandler and the response is sent", async () => {
    let handlerCalled: RpcRequest | null = null;
    transport.setHandler(async (req) => {
      handlerCalled = req;
      return { id: req.id, ok: true, result: { stub: true } };
    });
    transport.connect("ws://test.local/");
    await nextTick();
    const ws = getLastWs()!;
    ws.__sent.length = 0; // drop hello
    ws.__recv(JSON.stringify({ id: 9, method: "click", args: ["#x"] }));
    // Handler is async — give it a tick.
    await nextTick();
    expect(handlerCalled).not.toBeNull();
    expect(handlerCalled!.method).toBe("click");
    expect(JSON.parse(ws.__sent[0])).toEqual({ id: 9, ok: true, result: { stub: true } });
  });

  it("snapshot.rpcsReceived increments per inbound request", async () => {
    transport.setHandler(async (req) => ({ id: req.id, ok: true, result: 0 }));
    transport.connect("ws://test.local/");
    await nextTick();
    const ws = getLastWs()!;
    const before = transport.getSnapshot().rpcsReceived;
    ws.__recv(JSON.stringify({ id: 1, method: "schema", args: [] }));
    ws.__recv(JSON.stringify({ id: 2, method: "schema", args: [] }));
    await nextTick();
    expect(transport.getSnapshot().rpcsReceived).toBe(before + 2);
  });

  it("disconnect suppresses reconnect", async () => {
    transport.connect("ws://test.local/");
    await nextTick();
    expect(transport.getSnapshot().state).toBe("connected");
    transport.disconnect();
    expect(transport.getSnapshot().state).toBe("disconnected");
  });

  it("configure persists endpoint + auto in storage", async () => {
    await transport.configure("ws://other:9000/", true);
    const stored = await getShim().storage.local.get([
      "transport.endpoint",
      "transport.auto",
    ]);
    expect(stored["transport.endpoint"]).toBe("ws://other:9000/");
    expect(stored["transport.auto"]).toBe(true);
  });
});
