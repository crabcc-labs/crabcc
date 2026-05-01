// Test-only globals. Importing this file installs minimal `chrome` and
// `WebSocket` stand-ins on `globalThis` so the production modules can
// run under bun:test without a real Chrome runtime.
//
// Each test that mutates the shim should call `resetShim()` in beforeEach
// to drop registered listeners and pending fakes.

interface FakeListener<T extends unknown[]> {
  listeners: ((...args: T) => void)[];
  addListener: (cb: (...args: T) => void) => void;
  removeListener: (cb: (...args: T) => void) => void;
}

function makeEvent<T extends unknown[]>(): FakeListener<T> {
  const listeners: ((...args: T) => void)[] = [];
  return {
    listeners,
    addListener: (cb) => {
      listeners.push(cb);
    },
    removeListener: (cb) => {
      const idx = listeners.indexOf(cb);
      if (idx >= 0) listeners.splice(idx, 1);
    },
  };
}

function fire<T extends unknown[]>(ev: FakeListener<T>, ...args: T): void {
  for (const cb of ev.listeners) cb(...args);
}

export interface ChromeShim {
  scripting: {
    executeScript: (params: unknown) => Promise<{ result: unknown; error?: { message: string } }[]>;
    /** Test hook: set the next executeScript result. */
    __next: { result?: unknown; error?: { message: string }; throws?: Error } | null;
  };
  storage: {
    local: {
      __data: Record<string, unknown>;
      get: (keys: string[] | string | null) => Promise<Record<string, unknown>>;
      set: (data: Record<string, unknown>) => Promise<void>;
    };
    onChanged: FakeListener<[Record<string, unknown>, string]>;
  };
  tabs: {
    __tabs: Map<number, { id: number; url: string; title: string; windowId: number; status: string }>;
    get: (tabId: number) => Promise<{ id: number; url: string; title: string; windowId: number; status: string }>;
    query: (q: unknown) => Promise<{ id?: number; url?: string }[]>;
    captureVisibleTab: (windowId: number, opts: unknown) => Promise<string>;
    onRemoved: FakeListener<[number]>;
  };
  windows: { WINDOW_ID_CURRENT: -2 };
  runtime: {
    onMessage: FakeListener<[unknown, unknown, (m: unknown) => void]>;
    onInstalled: FakeListener<[]>;
    sendMessage: (msg: unknown) => Promise<unknown>;
  };
  debugger: {
    __attached: Set<number>;
    __commands: { tabId: number; method: string; params?: unknown }[];
    __nextCommand: Record<string, unknown>;
    attach: (target: { tabId: number }, version: string) => Promise<void>;
    detach: (target: { tabId: number }) => Promise<void>;
    sendCommand: (target: { tabId: number }, method: string, params?: unknown) => Promise<unknown>;
    onEvent: FakeListener<[{ tabId?: number }, string, unknown]>;
    onDetach: FakeListener<[{ tabId?: number }, string]>;
  };
}

function buildShim(): ChromeShim {
  return {
    scripting: {
      executeScript: async (_params: unknown) => {
        const next = shim.scripting.__next;
        shim.scripting.__next = null;
        if (next?.throws) throw next.throws;
        if (next?.error) return [{ result: undefined, error: next.error }];
        return [{ result: next?.result }];
      },
      __next: null,
    },
    storage: {
      local: {
        __data: {},
        get: async (keys) => {
          if (keys == null) return { ...shim.storage.local.__data };
          const arr = Array.isArray(keys) ? keys : [keys as string];
          const out: Record<string, unknown> = {};
          for (const k of arr) {
            if (k in shim.storage.local.__data) out[k] = shim.storage.local.__data[k];
          }
          return out;
        },
        set: async (data) => {
          const changes: Record<string, unknown> = {};
          for (const [k, v] of Object.entries(data)) {
            changes[k] = v;
            shim.storage.local.__data[k] = v;
          }
          fire(shim.storage.onChanged, changes, "local");
        },
      },
      onChanged: makeEvent<[Record<string, unknown>, string]>(),
    },
    tabs: {
      __tabs: new Map([
        [1, { id: 1, url: "https://example.com/", title: "Example", windowId: 100, status: "complete" }],
      ]),
      get: async (tabId) => {
        const t = shim.tabs.__tabs.get(tabId);
        if (!t) throw new Error(`no tab ${tabId}`);
        return t;
      },
      query: async (_q) => {
        return Array.from(shim.tabs.__tabs.values()).slice(0, 1).map((t) => ({ id: t.id, url: t.url }));
      },
      captureVisibleTab: async (_windowId, _opts) => {
        // 1×1 transparent PNG.
        return "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkAAIAAAoAAv/lxKUAAAAASUVORK5CYII=";
      },
      onRemoved: makeEvent<[number]>(),
    },
    windows: { WINDOW_ID_CURRENT: -2 },
    runtime: {
      onMessage: makeEvent<[unknown, unknown, (m: unknown) => void]>(),
      onInstalled: makeEvent<[]>(),
      sendMessage: async (_msg) => undefined,
    },
    debugger: {
      __attached: new Set<number>(),
      __commands: [],
      __nextCommand: {},
      attach: async (target, _v) => {
        shim.debugger.__attached.add(target.tabId);
      },
      detach: async (target) => {
        shim.debugger.__attached.delete(target.tabId);
      },
      sendCommand: async (target, method, params) => {
        shim.debugger.__commands.push({ tabId: target.tabId, method, params });
        return shim.debugger.__nextCommand[method] ?? {};
      },
      onEvent: makeEvent<[{ tabId?: number }, string, unknown]>(),
      onDetach: makeEvent<[{ tabId?: number }, string]>(),
    },
  };
}

const shim = buildShim();

export function getShim(): ChromeShim {
  return shim;
}

export function fireDebuggerEvent(tabId: number, method: string, params: unknown): void {
  fire(shim.debugger.onEvent, { tabId }, method, params);
}

export function fireTabRemoved(tabId: number): void {
  fire(shim.tabs.onRemoved, tabId);
}

/**
 * Wipe mutable state on the shim *in place* — never replace the shim
 * object itself. Modules under test capture references to the shim's
 * event objects (chrome.debugger.onEvent etc.) at first listener
 * registration, so swapping out the shim leaves modules listening to a
 * dead instance.
 *
 * Function-typed properties (`scripting.executeScript`, `tabs.captureVisibleTab`)
 * are also restored — tests sometimes monkey-patch them, and the next
 * test must start from a known function rather than a stale override.
 */
export function resetShim(): void {
  const fresh = buildShim();
  shim.scripting.__next = null;
  shim.scripting.executeScript = fresh.scripting.executeScript;
  shim.storage.local.__data = {};
  shim.storage.onChanged.listeners.length = 0;
  shim.tabs.__tabs.clear();
  shim.tabs.__tabs.set(1, {
    id: 1,
    url: "https://example.com/",
    title: "Example",
    windowId: 100,
    status: "complete",
  });
  shim.tabs.get = fresh.tabs.get;
  shim.tabs.query = fresh.tabs.query;
  shim.tabs.captureVisibleTab = fresh.tabs.captureVisibleTab;
  shim.debugger.__attached.clear();
  shim.debugger.__commands.length = 0;
  for (const k of Object.keys(shim.debugger.__nextCommand)) {
    delete shim.debugger.__nextCommand[k];
  }
  shim.debugger.attach = fresh.debugger.attach;
  shim.debugger.detach = fresh.debugger.detach;
  shim.debugger.sendCommand = fresh.debugger.sendCommand;
}

// --- WebSocket fake -------------------------------------------------------

export interface FakeWebSocket extends WebSocket {
  /** Test hook: simulate the server pushing a message. */
  __recv: (data: string) => void;
  /** Test hook: simulate the server closing the socket. */
  __close: () => void;
  /** Test hook: read everything send()ed by the SUT. */
  __sent: string[];
}

let lastSocket: FakeWebSocket | null = null;

export function getLastWs(): FakeWebSocket | null {
  return lastSocket;
}

class FakeWS {
  // Spec-compliant readyState constants — both static and instance, so
  // `WebSocket.OPEN` and `socket.OPEN` both resolve. Production code
  // typically uses the class form, which is what the wrapper had to drop.
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readonly CONNECTING = 0;
  readonly OPEN = 1;
  readonly CLOSING = 2;
  readonly CLOSED = 3;
  readyState = 0;
  url: string;
  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  __sent: string[] = [];

  constructor(url: string) {
    this.url = url;
    // The constructor is the simplest place to register the instance for
    // tests to grab, rather than wrapping the constructor — wrapping
    // loses the static `OPEN` / `CONNECTING` constants the SUT reads.
    lastSocket = this as unknown as FakeWebSocket;
    queueMicrotask(() => {
      this.readyState = 1;
      this.onopen?.(new Event("open"));
    });
  }
  send(data: string): void {
    this.__sent.push(data);
  }
  close(): void {
    this.readyState = 3;
    this.onclose?.(new Event("close"));
  }
  __recv(data: string): void {
    this.onmessage?.({ data });
  }
  __close(): void {
    this.readyState = 3;
    this.onclose?.(new Event("close"));
  }
}

function install(): void {
  (globalThis as unknown as { chrome: ChromeShim }).chrome = shim;
  (globalThis as unknown as { WebSocket: typeof FakeWS }).WebSocket = FakeWS;
}

install();
