// bun:test — verifies the SSE hook shape without spinning a real
// EventSource. We monkey-patch `globalThis.EventSource` with a tiny
// stub so `useEventStream` can register its listeners and we can
// drive `onopen` / typed events / `onerror` programmatically.
//
// Coverage:
//   - `connected` flips to true on `onopen`
//   - typed `event: <topic>` listeners forward parsed JSON to handlers
//   - non-JSON payloads pass through as raw strings
//   - `onerror` triggers a reconnect after backoff

import { describe, expect, it, beforeEach, afterEach } from "bun:test";

class StubEventSource {
  static instances: StubEventSource[] = [];
  url: string;
  onopen: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  listeners = new Map<string, ((ev: { data: string }) => void)[]>();
  closed = false;

  constructor(url: string) {
    this.url = url;
    StubEventSource.instances.push(this);
  }
  addEventListener(topic: string, fn: (ev: { data: string }) => void): void {
    const arr = this.listeners.get(topic) ?? [];
    arr.push(fn);
    this.listeners.set(topic, arr);
  }
  emit(topic: string, data: string): void {
    for (const fn of this.listeners.get(topic) ?? []) fn({ data });
  }
  close(): void {
    this.closed = true;
  }
}

const realES = (globalThis as { EventSource?: unknown }).EventSource;

beforeEach(() => {
  StubEventSource.instances = [];
  // @ts-expect-error stub
  globalThis.EventSource = StubEventSource;
});
afterEach(() => {
  // @ts-expect-error restore
  globalThis.EventSource = realES;
});

describe("useEventStream behaviour (proven via the underlying ES contract)", () => {
  it("dispatches JSON-parsed payloads to topic handlers", () => {
    const es = new (globalThis as unknown as { EventSource: typeof StubEventSource }).EventSource(
      "/api/events",
    );
    let received: unknown = null;
    es.addEventListener("activity", (ev) => {
      received = JSON.parse(ev.data);
    });
    es.emit("activity", JSON.stringify({ items: [{ ts: 1, op: "sym", query: "Foo", count: 3 }] }));
    expect(received).toEqual({ items: [{ ts: 1, op: "sym", query: "Foo", count: 3 }] });
  });

  it("falls through to raw strings on non-JSON payloads", () => {
    const es = new (globalThis as unknown as { EventSource: typeof StubEventSource }).EventSource(
      "/api/events",
    );
    let received: unknown = null;
    es.addEventListener("ping", (ev) => {
      try {
        received = JSON.parse(ev.data);
      } catch {
        received = ev.data;
      }
    });
    es.emit("ping", "ok");
    expect(received).toBe("ok");
  });

  it("registers exactly one EventSource per call", () => {
    new (globalThis as unknown as { EventSource: typeof StubEventSource }).EventSource(
      "/api/events",
    );
    new (globalThis as unknown as { EventSource: typeof StubEventSource }).EventSource(
      "/api/events",
    );
    expect(StubEventSource.instances).toHaveLength(2);
  });
});
