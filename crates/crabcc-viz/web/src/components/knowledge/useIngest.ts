// Wraps the POST /api/memory/ingest endpoint. Coalesces in-flight
// requests (only one at a time) and exposes an `abort()` so the UI's
// cancel button can drop a stuck fetch without leaking the hook.
//
// Wire shape mirrors the Rust handler. Kept hand-typed (rather than
// aliased through `api.gen.ts`) because the OpenAPI declares the body
// + response with `additionalProperties: true` — the codegen surfaces
// `unknown`, which would force casts at every use site.

import { useCallback, useRef, useState } from "react";

export interface IngestRequest {
  text?: string;
  urls?: string[];
  tags?: string[];
  source?: string;
}

export interface IngestItem {
  id: string;
  url?: string;
  title?: string;
  kind?: string;
  bytes: number;
  drawer_id: number;
}

export interface IngestError {
  url: string;
  error: string;
}

export interface IngestStats {
  ok: number;
  failed: number;
}

export interface IngestResult {
  ingested: IngestItem[];
  errors: IngestError[];
  stats: IngestStats;
}

export interface UseIngest {
  ingest: (req: IngestRequest) => Promise<IngestResult>;
  ingesting: boolean;
  result: IngestResult | null;
  error: string | null;
  abort: () => void;
  reset: () => void;
}

export function useIngest(): UseIngest {
  const [ingesting, setIngesting] = useState(false);
  const [result, setResult] = useState<IngestResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const ctlRef = useRef<AbortController | null>(null);

  const ingest = useCallback(async (req: IngestRequest): Promise<IngestResult> => {
    // Coalesce: if a previous request is in flight, abort it so we
    // never double-write to memory.db from two stuck clicks.
    ctlRef.current?.abort();
    const ctl = new AbortController();
    ctlRef.current = ctl;
    setIngesting(true);
    setError(null);
    try {
      const r = await fetch("/api/memory/ingest", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(req),
        signal: ctl.signal,
      });
      if (!r.ok) {
        const txt = await r.text().catch(() => "");
        throw new Error(`${r.status} ${r.statusText}${txt ? `: ${txt}` : ""}`);
      }
      const data = (await r.json()) as IngestResult;
      // Only commit state if this controller is still the active one
      // (another call may have superseded us via abort).
      if (ctlRef.current === ctl) {
        setResult(data);
        setError(null);
      }
      return data;
    } catch (e) {
      const err = e as Error;
      if (err.name === "AbortError") {
        if (ctlRef.current === ctl) setError("aborted");
        throw err;
      }
      if (ctlRef.current === ctl) setError(err.message);
      throw err;
    } finally {
      if (ctlRef.current === ctl) {
        setIngesting(false);
        ctlRef.current = null;
      }
    }
  }, []);

  const abort = useCallback(() => {
    ctlRef.current?.abort();
  }, []);

  const reset = useCallback(() => {
    setResult(null);
    setError(null);
  }, []);

  return { ingest, ingesting, result, error, abort, reset };
}
