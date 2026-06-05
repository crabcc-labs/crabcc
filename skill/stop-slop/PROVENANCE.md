# Provenance — vendored skill

`stop-slop` is **vendored** into this repo, not authored here.

- **Upstream:** <https://github.com/hardikpandya/stop-slop> (branch `main`)
- **Author:** Hardik Pandya — <https://hvpandya.com>
- **License:** MIT (see [`LICENSE`](LICENSE)) — preserved verbatim.
- **Vendored on:** 2026-06-05
- **Upstream files copied verbatim:** `SKILL.md`, `references/phrases.md`,
  `references/structures.md`, `references/examples.md`, `LICENSE`.

## Why it lives here

It applies to **all** agents working in this repo (Claude Code, Cursor,
Aider, Continue, … — see [`AGENTS.md`](../../AGENTS.md)), so it ships in-tree
like the other entries under `skill/` rather than being fetched per session.

## Updating

Re-pull the upstream files verbatim and bump the *Vendored on* date:

```bash
base=https://raw.githubusercontent.com/hardikpandya/stop-slop/main
for f in SKILL.md LICENSE references/phrases.md references/structures.md references/examples.md; do
  curl -fsSL "$base/$f" -o "skill/stop-slop/$f"
done
```

Keep upstream files byte-for-byte; put any crabcc-local notes here, not in them.
