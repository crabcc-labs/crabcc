// Inline unified diff viewer with +/− line highlighting.
// Keeps the bundle lean: no external diff library needed — the GitHub API
// already returns the unified patch string, we just tokenize it.

import { memo } from "react";
import { cn } from "../../lib/cn";
import type { PrFile } from "../../api";

type Props = {
  file: PrFile;
};

export const DiffViewer = memo(function DiffViewer({ file }: Props) {
  const patch = file.patch;

  return (
    <div className="rounded border border-border overflow-hidden text-xs font-mono">
      {/* File header */}
      <div
        className={cn(
          "flex items-center gap-2 px-3 py-1.5",
          "bg-card border-b border-border text-muted",
          file.status === "added" && "bg-success/5",
          file.status === "removed" && "bg-destructive/5",
        )}
      >
        <span
          className={cn(
            "shrink-0 w-4 h-4 rounded-sm flex items-center justify-center text-[9px] font-bold",
            file.status === "added" && "bg-success text-white",
            file.status === "removed" && "bg-destructive text-white",
            file.status === "modified" && "bg-primary text-white",
            file.status === "renamed" && "bg-[#8b5cf6] text-white",
          )}
        >
          {statusLetter(file.status)}
        </span>
        <span className="flex-1 truncate text-foreground">{file.filename}</span>
        {file.previous_filename && (
          <span className="text-muted truncate max-w-[200px]">
            ← {file.previous_filename}
          </span>
        )}
        <span className="shrink-0 text-success">+{file.additions}</span>
        <span className="shrink-0 text-destructive">−{file.deletions}</span>
      </div>

      {/* Diff lines */}
      {patch ? (
        <div className="overflow-x-auto max-h-96">
          {parsePatch(patch).map((line, i) => (
            <div
              key={i}
              className={cn(
                "flex whitespace-pre leading-5 px-1",
                line.type === "add" && "bg-success/10 text-success",
                line.type === "del" && "bg-destructive/10 text-destructive",
                line.type === "hunk" && "bg-muted/10 text-muted italic",
                line.type === "ctx" && "text-muted",
              )}
            >
              <span className="w-6 shrink-0 text-muted select-none text-right pr-2">
                {line.type === "add"
                  ? "+"
                  : line.type === "del"
                    ? "−"
                    : line.type === "hunk"
                      ? "@@"
                      : ""}
              </span>
              <span className="flex-1 min-w-0 break-all">{line.text}</span>
            </div>
          ))}
        </div>
      ) : (
        <div className="px-3 py-4 text-muted text-center">
          Diff not available (binary or large file)
        </div>
      )}
    </div>
  );
});

type LineType = "add" | "del" | "hunk" | "ctx";
interface DiffLine {
  type: LineType;
  text: string;
}

function parsePatch(patch: string): DiffLine[] {
  return patch.split("\n").map((raw) => {
    if (raw.startsWith("@@"))
      return { type: "hunk", text: raw.replace(/^@@[^@]*@@/, "").trim() };
    if (raw.startsWith("+")) return { type: "add", text: raw.slice(1) };
    if (raw.startsWith("-")) return { type: "del", text: raw.slice(1) };
    return { type: "ctx", text: raw.startsWith(" ") ? raw.slice(1) : raw };
  });
}

function statusLetter(status: string): string {
  return status === "added"
    ? "A"
    : status === "removed"
      ? "D"
      : status === "renamed"
        ? "R"
        : "M";
}
