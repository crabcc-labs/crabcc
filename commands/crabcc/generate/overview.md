---
description: Regenerate or refresh docs/OVERVIEW.md — colorful Mermaid diagrams for crabcc architecture (for humans and agents).
---

Update the visual architecture doc at `docs/OVERVIEW.md`.

## Goal

Produce a **diagram-heavy** overview: Mermaid flowcharts, sequence diagrams, mindmaps, and charts with explicit `themeVariables` / `classDef` colors so renders look good on GitHub and in Claude Code.

Do **not** duplicate the full README. Link out for install steps, bench tables, and long prose traces.

## Steps

1. Read current sources of truth:
   - `README.md` (especially § Architecture, Usage, Ollama stack)
   - `crates/crabcc-core/docs/HOW_IT_WORKS.md`
   - `AGENTS.md`
   - Root `Cargo.toml` (`members` / `exclude`)
   - `install/integrations.md` if integration section needs refresh

2. Open `docs/OVERVIEW.md`. Preserve section numbering where possible; update diagrams when crates, paths, or flows changed.

3. Required diagram sections (use Mermaid with `%%{init: ...}%%` color themes):
   - System context (agents → CLI/MCP → libs → disk)
   - Workspace crate map (workspace vs standalone apps)
   - On-disk layout (`.crabcc/` vs `$CRABCC_HOME`)
   - Indexing pipeline
   - Query router decision tree
   - Memory hybrid / RRF
   - Agent integrations mindmap
   - Ollama stack sequence
   - CLI vs MCP dual path
   - Doc map (links to README, HOW_IT_WORKS, AGENTS, examples)

4. Add or refresh tables only where they clarify a diagram (paths, commands, “which store”).

5. Fix factual drift vs README (e.g. memory path under `$CRABCC_HOME/repos/<slug>-<hash6>/`, not `<repo>/.crabcc/memory.db`).

6. At the top of `docs/OVERVIEW.md`, keep a one-line pointer to this command for future regen.

7. Update cross-links if you add sections:
   - `README.md` table of contents → link **Visual overview**
   - `AGENTS.md` → link under “What this repo is”
   - `docs/README.md` → list `OVERVIEW.md` in the index table

8. Report to the user: which sections changed and any architectural assumptions you could not verify (list files you could not read).

## Style rules

- Prefer Mermaid over ASCII for new art; keep one short ASCII block only if Mermaid cannot express it.
- Use `classDef` / `style` for color; avoid walls of plain bullet lists.
- Command names in `backticks`; paths literal.
- No secrets, env values, or API keys in diagrams.

## CLI-only

This workflow is file edits + `git diff` — no special API. Optional verify: open `docs/OVERVIEW.md` preview in the IDE.
