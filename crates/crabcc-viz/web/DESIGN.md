# `/live` React frontend — design

> Tracks issue #17. Replaces the bundled `assets/live.html` (single-file
> static page, ~1000 lines of inline JS) with a React + TypeScript app
> built via **bun + esbuild** and emitted as a single self-contained
> HTML the Rust crate inlines via `include_str!` — same shipping shape,
> richer codebase.

## Why now

The current `live.html` is hand-rolled DOM manipulation. It works, but:

- **Per-frame churn** — `pollAgents` + `pollActivity` + `pollMemory` all
  re-render their full subtrees on every tick. At ≥3 active agents the
  hand-rolled `innerHTML` rebuilds dominate frame time.
- **No diffing** — log tails grow unbounded; the textarea-based agent
  log view stutters past ~10 KB.
- **Tight coupling between data + DOM** — adding a new panel means
  copy-pasting the polling loop pattern.

A small React app behind the same JSON endpoints fixes all three with
a 0 ms-to-interactive bundle (≤ 80 KB gzipped target).

## Build pipeline

```
bun install
bun run build         # esbuild → dist/live.html (single file)
```

- **bun** as package manager + script runner — already on the user's
  machine; installs are ~5× faster than npm.
- **esbuild** for transpilation + bundling. Single binary, no plugin
  zoo. Inlines CSS into the HTML; emits one file.
- **typescript-go** (`tsgo`) for type-check, falling back to vanilla
  `tsc --noEmit` if `tsgo` isn't available. typescript-go is preview
  software at time of writing; the build doesn't *depend* on it.
- Output goes to `crates/crabcc-viz/web/dist/live.html` and is
  committed to the repo so `cargo build` doesn't need bun. The Rust
  crate `include_str!`s it the same way it does the hand-written
  `live.html` today.

## Scope

This PR (phase 1):

- Set up the `web/` workspace: `package.json`, `tsconfig.json`,
  `esbuild.config.mjs`, source skeleton.
- Port the **header** (brand, live indicator, action buttons including
  ↻ Re-index PWD from #101).
- Port the **activity panel** (left column — recent tool calls).
- Port the **agents panel** (right column tab) with live log tail.
- Wire the existing API endpoints (`/api/bootstrap`, `/api/activity`,
  `/api/agents`, `/api/reindex`).
- Keep the existing `assets/live.html` as a fallback for one release.
- Build script + Taskfile target + CI gate that compares the React
  bundle against the legacy file and asserts both stay reachable.

Phases deferred (tracked in a follow-up issue):

- **Phase 2** — port the relations graph (currently vis-network); pick
  between react-vis-network and a thin imperative wrapper.
- **Phase 3** — port the launch / debug panels.
- **Phase 4** — virtualized lists for the activity panel + log view
  (react-window or a hand-rolled tail-N+resize-observer).
- **Phase 5** — JSON optimization: switch the polling layer to
  pre-validated typed responses + structural sharing so React's
  reference-equality bail-outs kick in. Optionally wire SIMD-JSON in
  the Rust side response path (`sonic-rs::to_writer` is already there).
- **Phase 6** — replace polling with Server-Sent Events (already in the
  v3.0 sandbox seam roadmap).
- **Phase 7** — flip `assets/live.html` to be the React bundle; delete
  the legacy single-file dashboard.

## Performance budget

| Metric | Target | Today (legacy) |
|---|---|---|
| Bundle size (gzipped) | ≤ 80 KB | n/a (HTML 41 KB raw) |
| TTI on local dashboard | < 50 ms | ~30 ms |
| Frame budget at 3 agents + 30 activity rows | 16 ms | ~25–40 ms (occasional drops) |
| JSON parse on `/api/activity` poll | ≤ 0.5 ms | ~0.3 ms (small response) |

If a metric regresses against the legacy implementation, the React
build is gated off in CI until it's addressed.

## Open questions (resolve in PR review)

1. **react-vis-network vs. wrapper**: vis-network is imperative; React
   integration libraries are unmaintained. Probably easier to write a
   thin imperative `useEffect` wrapper than depend on a stale package.
2. **SSE upgrade timing**: phase 6 above could collapse three polling
   loops into one event stream; do it in this rewrite or follow-up?
3. **typescript-go fallback**: keep both `tsgo` and `tsc` toolchains in
   the package.json scripts, or drop `tsc` once `tsgo` GA's?
