// Component tests for <IngestBox /> — proves the happy-dom + React
// 19 wiring works end-to-end and pins the contract the orchestrator
// depends on (textarea + read button, in-flight cancel, result card).

import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { actAsync, render, setInputValue } from "../../../test/render";
import { IngestBox } from "./IngestBox";

const ENDPOINT = "/api/memory/ingest";

interface MockFetch {
  calls: Array<{ url: string; init?: RequestInit }>;
  resolve: (json: unknown) => void;
  reject: (err: Error) => void;
  promise: Promise<unknown>;
  abortedSignals: AbortSignal[];
}

function installMockFetch(): MockFetch {
  const calls: MockFetch["calls"] = [];
  const abortedSignals: AbortSignal[] = [];
  let resolve!: (j: unknown) => void;
  let reject!: (e: Error) => void;
  const promise = new Promise<unknown>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  (globalThis as unknown as { fetch: typeof fetch }).fetch = (async (
    input: string | URL | Request,
    init?: RequestInit,
  ) => {
    const url = typeof input === "string" ? input : input.toString();
    calls.push({ url, init });
    const signal = init?.signal;
    if (signal) {
      signal.addEventListener("abort", () => abortedSignals.push(signal));
    }
    const json = (await promise) as unknown;
    return new Response(JSON.stringify(json), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    }) as unknown as Response;
  }) as unknown as typeof fetch;
  return { calls, resolve, reject, promise, abortedSignals };
}

const ORIGINAL_FETCH = globalThis.fetch;

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = ORIGINAL_FETCH;
});

beforeEach(() => {
  document.body.innerHTML = "";
});

describe("<IngestBox />", () => {
  it("renders textarea + read button", () => {
    const r = render(<IngestBox />);
    const ta = r.container.querySelector(
      "[data-testid=ingest-textarea]",
    ) as HTMLTextAreaElement | null;
    const btn = r.container.querySelector(
      "[data-testid=ingest-read]",
    ) as HTMLButtonElement | null;
    expect(ta).not.toBeNull();
    expect(btn).not.toBeNull();
    r.unmount();
  });

  it("disables read button when textarea is empty", () => {
    const r = render(<IngestBox />);
    const btn = r.container.querySelector(
      "[data-testid=ingest-read]",
    ) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    r.unmount();
  });

  it("enables read button once user types and submits to /api/memory/ingest", async () => {
    const mock = installMockFetch();
    const r = render(<IngestBox />);
    const ta = r.container.querySelector(
      "[data-testid=ingest-textarea]",
    ) as HTMLTextAreaElement;
    const btn = r.container.querySelector(
      "[data-testid=ingest-read]",
    ) as HTMLButtonElement;

    await actAsync(() => {
      setInputValue(ta, "https://example.com");
    });
    expect(btn.disabled).toBe(false);

    await actAsync(() => {
      btn.click();
    });
    expect(mock.calls.length).toBe(1);
    expect(mock.calls[0].url).toBe(ENDPOINT);
    const body = JSON.parse(String(mock.calls[0].init?.body ?? "{}")) as {
      text: string;
    };
    expect(body.text).toBe("https://example.com");

    // Resolve the in-flight request to settle the test cleanly.
    await actAsync(async () => {
      mock.resolve({
        ingested: [],
        errors: [],
        stats: { ok: 0, failed: 0 },
      });
      await Promise.resolve();
    });
    r.unmount();
  });

  it("shows cancel button while a request is in flight, hides read", async () => {
    const mock = installMockFetch();
    const r = render(<IngestBox />);
    const ta = r.container.querySelector(
      "[data-testid=ingest-textarea]",
    ) as HTMLTextAreaElement;
    await actAsync(() => {
      setInputValue(ta, "https://example.com");
    });
    const btn = r.container.querySelector(
      "[data-testid=ingest-read]",
    ) as HTMLButtonElement;
    await actAsync(() => {
      btn.click();
    });

    // Inflight: spinner + cancel visible, read gone.
    expect(
      r.container.querySelector("[data-testid=ingest-spinner]"),
    ).not.toBeNull();
    expect(
      r.container.querySelector("[data-testid=ingest-cancel]"),
    ).not.toBeNull();
    expect(
      r.container.querySelector("[data-testid=ingest-read]"),
    ).toBeNull();

    // Cancel aborts the controller.
    const cancel = r.container.querySelector(
      "[data-testid=ingest-cancel]",
    ) as HTMLButtonElement;
    await actAsync(() => {
      cancel.click();
    });
    expect(mock.abortedSignals.length).toBe(1);

    // Tear down: reject the still-pending promise so awaits inside
    // useIngest resolve.
    await actAsync(async () => {
      mock.reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
      await Promise.resolve();
    });
    r.unmount();
  });

  it("renders the result card with ingested + errors after a request", async () => {
    const mock = installMockFetch();
    const r = render(<IngestBox />);
    const ta = r.container.querySelector(
      "[data-testid=ingest-textarea]",
    ) as HTMLTextAreaElement;
    await actAsync(() => {
      setInputValue(ta, "hello world");
    });
    const btn = r.container.querySelector(
      "[data-testid=ingest-read]",
    ) as HTMLButtonElement;
    await actAsync(() => {
      btn.click();
    });
    await actAsync(async () => {
      mock.resolve({
        ingested: [
          {
            id: "text:abc",
            kind: "text",
            bytes: 11,
            drawer_id: 7,
          },
          {
            id: "web:xyz",
            url: "https://example.com",
            title: "Example",
            bytes: 1024,
            drawer_id: 8,
          },
        ],
        errors: [{ url: "http://bad", error: "timeout" }],
        stats: { ok: 2, failed: 1 },
      });
      // Microtask drain for the .then in useIngest.
      await Promise.resolve();
      await Promise.resolve();
    });

    const results = r.container.querySelector(
      "[data-testid=ingest-results]",
    ) as HTMLElement | null;
    expect(results).not.toBeNull();
    expect(results!.textContent).toContain("2 ok");
    expect(results!.textContent).toContain("1 failed");
    expect(results!.textContent).toContain("Example");
    expect(results!.textContent).toContain("text:abc");
    expect(results!.textContent).toContain("timeout");
    r.unmount();
  });

  it("invokes onSelect with doc:<drawer_id> when a result is clicked", async () => {
    const mock = installMockFetch();
    const recv: { id: string | null } = { id: null };
    const r = render(<IngestBox onSelect={(id) => { recv.id = id; }} />);
    const ta = r.container.querySelector(
      "[data-testid=ingest-textarea]",
    ) as HTMLTextAreaElement;
    await actAsync(() => {
      setInputValue(ta, "x");
    });
    const btn = r.container.querySelector(
      "[data-testid=ingest-read]",
    ) as HTMLButtonElement;
    await actAsync(() => {
      btn.click();
    });
    await actAsync(async () => {
      mock.resolve({
        ingested: [
          { id: "web:xyz", url: "https://example.com", title: "X", bytes: 1, drawer_id: 42 },
        ],
        errors: [],
        stats: { ok: 1, failed: 0 },
      });
      await Promise.resolve();
      await Promise.resolve();
    });
    const link = r.container.querySelector(
      ".knowledge-ingest-link",
    ) as HTMLButtonElement;
    await actAsync(() => {
      link.click();
    });
    expect(recv.id).toBe("doc:42");
    r.unmount();
  });
});
