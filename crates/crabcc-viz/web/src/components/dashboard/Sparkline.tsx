// Hand-rolled sparkline — no chart lib. The dashboard's events/min KPI
// renders ~24 vertical bars whose height tracks bucket count. Pure DOM,
// CSS-styled, no SVG: keeps the bundle bytes nil and the render cost
// dominated by React reconciliation rather than a library tree.

import { memo } from "react";
import type { SparkBucket } from "./selectors";

interface Props {
  buckets: SparkBucket[];
  /** Maximum bar height in CSS pixels. Defaults to 32. */
  height?: number;
}

export const Sparkline = memo(function Sparkline({ buckets, height = 32 }: Props) {
  const peak = Math.max(1, ...buckets.map((b) => b.count));
  return (
    <div className="dash-spark" style={{ height }} aria-hidden="true">
      {buckets.map((b, i) => {
        // Floor at 8% so an empty bucket still has a visible baseline
        // tick — the row otherwise looks like a stretch of dead space
        // when activity is bursty.
        const pct = b.count === 0 ? 0.08 : 0.18 + 0.82 * (b.count / peak);
        return (
          <span
            key={i}
            className={`dash-spark-bar${b.count > 0 ? " has-events" : ""}`}
            style={{ height: `${pct * 100}%` }}
          />
        );
      })}
    </div>
  );
});
