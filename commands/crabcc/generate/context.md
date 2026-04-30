---
description: Generate an optimized repo context bundle (compressed XML + symbol index + call graph) and copy it to the system clipboard.
---

Generate the bundle and place it on the clipboard.

## Steps

1. Run `task repomix-context`. This:
   - Packs the repo with `repomix --compress` (tree-sitter extraction
     of class/function signatures, drops bodies — token-minimized).
   - Refreshes the crabcc symbol index (`crabcc refresh`) and rebuilds
     the call-graph sidecar (`crabcc graph build`).
   - Appends two JSONL sections to the bundle:
     - `<crabcc-index>` — one symbol per line (kind, signature, file,
       line range, parent).
     - `<crabcc-graph>` — one edge per line (caller, callee).
   - Copies the combined bundle to the clipboard via `pbcopy` /
     `wl-copy` / `xclip`.

2. Report the final bundle size to the user (printed by the task).

3. Tell the user the bundle is on their clipboard — they can paste it
   into any LLM chat surface (Claude, ChatGPT, Gemini). The model gets
   compressed source plus structural metadata it can navigate without
   burning tokens on full bodies.

## Variants

- Custom output path: `task repomix-context OUT=path/to/bundle.xml`.
- Single crate (lighter bundle): `task repomix-crate CRATE=<name>
  COMPRESS=1 COPY=1` is the underlying call; the structural index +
  graph append is full-repo only today (follow-up: scope to crate).

## CLI-only — confirmed possible

Every step is shell-driven: `repomix`, `crabcc`, `jq`, and the host
clipboard tool. No programmatic API access required. The "is it
possible?" answer is yes — see `Taskfile.yml` → `repomix-context`
for the full pipeline.
