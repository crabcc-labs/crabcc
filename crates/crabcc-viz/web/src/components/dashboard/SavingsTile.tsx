// `<SavingsTile />` — the live token-savings banner (#648).
//
// Full-width block at the top of the dashboard: the headline number is
// all-time tokens saved by the crabcc hooks (rewrite -> rg/refs, cat ->
// read, image downscale, Morph compaction), with 24h + session figures
// and a per-op bar breakdown. Polls /api/savings ~1Hz so the operator
// can watch the reduction climb live.

import { memo } from "react";
import { PiggyBank } from "lucide-react";
import { DashTile } from "./DashTile";
import { useSavings } from "./useSavings";

function fmtTok(n: number | undefined): string {
  const v = n ?? 0;
  if (v >= 1e6) return `${(v / 1e6).toFixed(1).replace(/\.0$/, "")}M`;
  if (v >= 1e3) return `${(v / 1e3).toFixed(1).replace(/\.0$/, "")}k`;
  return String(v);
}

export const SavingsTile = memo(function SavingsTile() {
  const { savings, loading } = useSavings();

  const ops = Object.entries(savings?.by_op ?? {})
    .map(([op, b]) => ({ op, saved: b.saved_tokens ?? 0 }))
    .filter((o) => o.saved > 0)
    .sort((a, b) => b.saved - a.saved);
  const max = ops.reduce((m, o) => Math.max(m, o.saved), 1);

  return (
    <DashTile
      title="tokens saved"
      icon={PiggyBank}
      area="saved"
      openHref="#/logs"
      openLabel="activity"
      meta={
        <span className="dash-pill">
          {fmtTok(savings?.all_time.queries)} ops
        </span>
      }
    >
      <div className="dash-saved">
        <div className="dash-saved-kpis">
          <div className="dash-saved-big">
            <span className="dash-saved-num">
              {loading && !savings ? "—" : fmtTok(savings?.all_time.saved_tokens)}
            </span>
            <span className="dash-saved-lbl">all-time</span>
          </div>
          <div className="dash-saved-sub">
            <span>
              <b>{fmtTok(savings?.last_24h.saved_tokens)}</b> 24h
            </span>
            <span>
              <b>{fmtTok(savings?.session.saved_tokens)}</b> session
            </span>
          </div>
        </div>
        <div className="dash-saved-ops">
          {ops.length === 0 ? (
            <span className="dash-empty">no savings recorded yet</span>
          ) : (
            ops.map((o) => (
              <div key={o.op} className="dash-saved-op" title={`${o.op}: ${o.saved} tok`}>
                <span className="dash-saved-op-name">{o.op}</span>
                <span className="dash-saved-op-bar">
                  <span style={{ width: `${Math.round((100 * o.saved) / max)}%` }} />
                </span>
                <span className="dash-saved-op-val">{fmtTok(o.saved)}</span>
              </div>
            ))
          )}
        </div>
      </div>
    </DashTile>
  );
});
