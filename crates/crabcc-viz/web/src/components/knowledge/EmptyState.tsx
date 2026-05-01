// Empty-state hint when memory.db has zero captured drawers. The
// canonical bootstrap command is `crabcc memory mine project` — we
// surface it copy-able so the user can paste straight into a terminal.

import { useState } from "react";

const SEED_CMD = "crabcc memory mine project";

export function EmptyState() {
  const [copied, setCopied] = useState(false);
  return (
    <div className="knowledge-empty">
      <h3>no memory drawers yet</h3>
      <p>
        The knowledge graph visualizes references between memory
        drawers. Seed your repo's memory db by running:
      </p>
      <div className="knowledge-empty-cmd">
        <code>{SEED_CMD}</code>
        <button
          type="button"
          onClick={() => {
            // navigator.clipboard is gated on secure contexts; the dashboard
            // runs on http://127.0.0.1, which counts as secure for the API.
            navigator.clipboard?.writeText(SEED_CMD).then(
              () => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1500);
              },
              () => setCopied(false),
            );
          }}
          aria-label="Copy command"
        >
          {copied ? "copied" : "copy"}
        </button>
      </div>
      <p className="knowledge-empty-hint">
        After mining, return here and reload the page — or paste content
        above to seed the knowledge base directly.
      </p>
    </div>
  );
}
