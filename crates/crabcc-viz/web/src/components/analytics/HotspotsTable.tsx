import { memo } from "react";
import { cn } from "../../lib/cn";
import type { HotspotFile } from "../../api";

type Props = {
  hotspots: HotspotFile[];
};

const MAX_BAR = 1;

export const HotspotsTable = memo(function HotspotsTable({ hotspots }: Props) {
  const maxCommits = Math.max(...hotspots.map((h) => h.commits), 1);

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-border text-muted">
            <th className="text-left py-1.5 pr-2 font-semibold w-1/2">File</th>
            <th className="text-right py-1.5 pr-3 font-semibold">Commits</th>
            <th className="text-right py-1.5 pr-3 font-semibold">Authors</th>
            <th className="text-left py-1.5 font-semibold w-20">Churn</th>
            <th className="text-right py-1.5 font-semibold">Last touched</th>
          </tr>
        </thead>
        <tbody>
          {hotspots.map((h) => (
            <tr
              key={h.file}
              className="border-b border-border/50 hover:bg-card/60 transition-colors"
            >
              <td className="py-1.5 pr-2 max-w-[240px]">
                <span
                  className="block truncate text-foreground font-mono"
                  title={h.file}
                >
                  {shortPath(h.file)}
                </span>
                <span className="text-muted text-[10px] truncate block" title={h.file}>
                  {h.file.length > 50 ? "…" + h.file.slice(-40) : ""}
                </span>
              </td>
              <td className="text-right pr-3 font-semibold text-primary">
                {h.commits}
              </td>
              <td className="text-right pr-3 text-muted">{h.authors}</td>
              <td className="py-1.5">
                <div
                  className="h-2 rounded-sm bg-primary/80"
                  style={{
                    width: `${Math.round((h.commits / maxCommits) * 100)}%`,
                    minWidth: "4px",
                  }}
                />
              </td>
              <td className="text-right text-muted pl-2">{h.last_seen}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
});

function shortPath(path: string): string {
  const parts = path.split("/");
  if (parts.length <= 3) return path;
  return `…/${parts.slice(-2).join("/")}`;
}
