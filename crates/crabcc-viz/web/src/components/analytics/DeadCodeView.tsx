import { memo } from "react";
import { AlertTriangle } from "lucide-react";
import type { DeadSymbol } from "../../api";
import { cn } from "../../lib/cn";

type Props = {
  symbols: DeadSymbol[];
};

export const DeadCodeView = memo(function DeadCodeView({ symbols }: Props) {
  if (symbols.length === 0) {
    return (
      <div className="py-8 text-center text-muted text-sm">
        No dead code detected — or run{" "}
        <code className="text-xs">crabcc graph build</code> to populate the
        call graph.
      </div>
    );
  }

  // Group by file for readability.
  const byFile: Record<string, DeadSymbol[]> = {};
  for (const sym of symbols) {
    (byFile[sym.file] ??= []).push(sym);
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2 text-xs text-muted">
        <AlertTriangle size={13} className="text-[#f59e0b]" />
        <span>
          {symbols.length} unreachable symbol{symbols.length !== 1 ? "s" : ""}{" "}
          (functions/methods with no callers in the call graph)
        </span>
      </div>
      {Object.entries(byFile).map(([file, syms]) => (
        <div key={file} className="border border-border rounded">
          <div className="px-3 py-1.5 bg-card border-b border-border text-xs text-muted font-mono truncate">
            {file}
          </div>
          <div className="divide-y divide-border/50">
            {syms.map((s) => (
              <div
                key={`${s.name}:${s.line}`}
                className="flex items-center gap-2 px-3 py-1.5 text-xs hover:bg-card/60"
              >
                <span
                  className={cn(
                    "shrink-0 w-14 text-center rounded-sm px-1 py-px text-[9px] font-semibold",
                    s.kind === "function"
                      ? "bg-primary/15 text-primary"
                      : "bg-muted/20 text-muted",
                  )}
                >
                  {s.kind}
                </span>
                <span className="font-mono text-foreground truncate flex-1">
                  {s.name}
                </span>
                <span className="shrink-0 text-muted text-[10px]">
                  :{s.line}
                </span>
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
});
