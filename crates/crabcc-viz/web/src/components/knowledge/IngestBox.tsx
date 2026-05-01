// IngestBox — the "paste URLs and freeform text, hit read" affordance
// at the top of the knowledge view. Wraps the `useIngest()` hook + a
// thin form. Result rendering is inline (small results card below the
// textarea). Selecting a result fires the `onSelect` prop so the
// orchestrator can navigate the graph to that drawer.
//
// Tags are split on commas; the `source` input is optional and falls
// through to the backend default (`web-ingest`) when blank.

import { useCallback, useState } from "react";
import { useIngest, type IngestItem } from "./useIngest";

interface Props {
  /** Called once per successful ingest so the parent can refresh
   * the graph snapshot. The argument is the full ingest result. */
  onIngested?: (result: { ingested: IngestItem[] }) => void;
  /** Called when the user clicks an ingested item; the parent uses
   * this to navigate the graph to that drawer. The id is the
   * server-side public id (`web:<sha>` or `text:<sha>`), which the
   * graph node uses too. */
  onSelect?: (id: string) => void;
}

export function IngestBox({ onIngested, onSelect }: Props) {
  const [text, setText] = useState("");
  const [tagsRaw, setTagsRaw] = useState("");
  const [source, setSource] = useState("");
  const { ingest, ingesting, result, error, abort, reset } = useIngest();

  const submit = useCallback(async () => {
    if (!text.trim()) return;
    const tags = tagsRaw
      .split(",")
      .map((t) => t.trim())
      .filter((t) => t.length > 0);
    try {
      const out = await ingest({
        text,
        tags,
        source: source.trim() || undefined,
      });
      if (onIngested) onIngested({ ingested: out.ingested });
      // Don't clear the textarea — the user may want to see what they
      // pasted alongside the result. Click "clear" if they want a
      // fresh slate.
    } catch {
      // useIngest sets `error`; nothing more to do here.
    }
  }, [text, tagsRaw, source, ingest, onIngested]);

  const onKey = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      // Cmd/Ctrl-Enter submits — same as most chat boxes. Plain Enter
      // inserts a newline so multi-URL pastes stay readable.
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !ingesting) {
        e.preventDefault();
        void submit();
      }
    },
    [submit, ingesting],
  );

  const empty = text.trim().length === 0;

  return (
    <section className="knowledge-ingest" aria-label="Ingest URLs or text">
      <textarea
        className="knowledge-ingest-textarea"
        placeholder="paste URLs or freeform text — one URL per line works, or mix them in prose"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKey}
        rows={4}
        spellCheck={false}
        aria-label="URLs or text to ingest"
        data-testid="ingest-textarea"
      />
      <div className="knowledge-ingest-row">
        <input
          className="knowledge-ingest-tags"
          type="text"
          placeholder="tags (comma-separated)"
          value={tagsRaw}
          onChange={(e) => setTagsRaw(e.target.value)}
          aria-label="Tags"
          data-testid="ingest-tags"
        />
        <input
          className="knowledge-ingest-source"
          type="text"
          placeholder="source label (optional)"
          value={source}
          onChange={(e) => setSource(e.target.value)}
          aria-label="Source label"
          data-testid="ingest-source"
        />
        <div className="knowledge-ingest-actions">
          {!ingesting && (
            <button
              type="button"
              className="knowledge-ingest-read"
              onClick={() => void submit()}
              disabled={empty}
              data-testid="ingest-read"
              aria-label="Read and ingest"
            >
              read
            </button>
          )}
          {ingesting && (
            <>
              <span
                className="knowledge-ingest-spin"
                aria-label="Ingesting"
                data-testid="ingest-spinner"
              />
              <button
                type="button"
                className="knowledge-ingest-cancel"
                onClick={abort}
                data-testid="ingest-cancel"
                aria-label="Cancel ingest"
              >
                cancel
              </button>
            </>
          )}
        </div>
      </div>
      {error && (
        <p className="knowledge-ingest-error" data-testid="ingest-error">
          {error}
        </p>
      )}
      {result && (
        <IngestResults
          result={result}
          onSelect={onSelect}
          onDismiss={reset}
        />
      )}
    </section>
  );
}

interface ResultsProps {
  result: { ingested: IngestItem[]; errors: { url: string; error: string }[]; stats: { ok: number; failed: number } };
  onSelect?: (id: string) => void;
  onDismiss: () => void;
}

function IngestResults({ result, onSelect, onDismiss }: ResultsProps) {
  return (
    <div className="knowledge-ingest-results" data-testid="ingest-results">
      <header>
        <strong>
          {result.stats.ok} ok · {result.stats.failed} failed
        </strong>
        <button
          type="button"
          className="knowledge-ingest-dismiss"
          onClick={onDismiss}
          aria-label="Dismiss results"
        >
          ×
        </button>
      </header>
      {result.ingested.length > 0 && (
        <ul className="knowledge-ingest-ok">
          {result.ingested.map((it) => (
            <li key={it.id}>
              <button
                type="button"
                className="knowledge-ingest-link"
                onClick={() => onSelect?.(`doc:${it.drawer_id}`)}
                title={it.url ?? it.id}
              >
                <code>{it.id}</code>
                <span className="knowledge-ingest-title">
                  {it.title ?? it.kind ?? "(text)"}
                </span>
                <span className="knowledge-ingest-bytes">
                  {it.bytes.toLocaleString()} B
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {result.errors.length > 0 && (
        <ul className="knowledge-ingest-errs">
          {result.errors.map((e, i) => (
            <li key={`${e.url}-${i}`}>
              <code>{e.url || "(text)"}</code>: {e.error}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
