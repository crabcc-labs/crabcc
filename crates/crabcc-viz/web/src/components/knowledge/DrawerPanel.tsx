// Side panel — renders the selected drawer's full body. We don't pull
// in a markdown lib (the project's preference is to render bodies as
// preformatted text and call it good enough for now). Mono font + soft
// wrap keeps URLs / code snippets readable without horizontal scroll.

import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { Icon } from "../icons";
import { fetchDrawer } from "./useKnowledgeData";
import type { DrawerDetail } from "./types";

interface Props {
  id: string | null;
  onClose: () => void;
}

export function DrawerPanel({ id, onClose }: Props) {
  const [data, setData] = useState<DrawerDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!id) {
      setData(null);
      setErr(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setErr(null);
    fetchDrawer(id)
      .then((d) => {
        if (!cancelled) setData(d);
      })
      .catch((e: Error) => {
        if (!cancelled) setErr(e.message);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id]);

  if (!id) return null;

  return (
    <aside className="knowledge-panel">
      <header className="knowledge-panel-head">
        <code title={id}>{id}</code>
        <button
          className="knowledge-panel-close"
          onClick={onClose}
          aria-label="Close panel"
          title="Close (Esc)"
        ><Icon of={X} size={12} /></button>
      </header>
      {loading && <p className="knowledge-panel-meta">loading…</p>}
      {err && <p className="knowledge-panel-meta error">{err}</p>}
      {data && data.found && (
        <>
          <dl className="knowledge-panel-meta-grid">
            <dt>wing</dt>
            <dd>{data.wing}</dd>
            {data.room && (
              <>
                <dt>room</dt>
                <dd>{data.room}</dd>
              </>
            )}
            <dt>source</dt>
            <dd className="knowledge-panel-source" title={data.source_id}>
              {data.source_id}
            </dd>
            <dt>created</dt>
            <dd>{new Date(data.created_at * 1000).toLocaleString()}</dd>
            <dt>length</dt>
            <dd>{data.body.length.toLocaleString()} chars</dd>
          </dl>
          <pre className="knowledge-panel-body">{data.body}</pre>
        </>
      )}
      {data && !data.found && !loading && !err && (
        <p className="knowledge-panel-meta">drawer not found</p>
      )}
    </aside>
  );
}
