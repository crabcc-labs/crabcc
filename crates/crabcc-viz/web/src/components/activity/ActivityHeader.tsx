// Sticky time-bucket / pinned-block header. Position-sticky inside
// the absolute-positioned virtual list — works because the container
// doesn't transform, only the rows shift via `top` offsets.

import { memo } from "react";

export const ActivityHeader = memo(function ActivityHeader({
  label,
}: {
  label: string;
}) {
  return <div className="activity-header" data-label={label}>{label}</div>;
});
