# Bundle analysis — `crates/crabcc-viz/web/dist/`

Generated from the esbuild metafile (`dist/metafile.json`) after the
React perf optimization pass. Run `bun run build` to regenerate the
metafile.

## Output chunks (production, minified, post-split)

| Chunk                          | Bytes  | Purpose                                                  |
| ------------------------------ | ------ | -------------------------------------------------------- |
| `main.js`                      | 245 KB | Entry — React + dashboard shell + activity/agents/tel.   |
| `chunk-RXWF5L5I.js` (shared)   | 19 KB  | d3-force simulation (shared by graph + knowledge views)  |
| `chunk-GR3WMLSL.js` (shared)   |  8 KB  | GraphCanvas + force layout primitives (shared)           |
| `chunk-YUJ36UPX.js` (shared)   |  4 KB  | Misc shared helpers (graph store + types)                |
| `RelationsGraph-XXX.js`        |  8 KB  | Lazy: dashboard's force-graph orchestrator               |
| `knowledge-XXX.js`             | 12 KB  | Lazy: `/#/knowledge` view (memory drawers + ingest UI)   |
| `main.css`                     | 37 KB  | All app styles (inlined into `live.html`)                |

`live.html` itself is now 37 KB (HTML shell + inlined CSS + a single
`<script type="module" src="/static/main.js">`). Pre-optimization it
was a 339 KB monolith with the JS + CSS inlined.

## Top 10 inputs (raw bytes, before tree-shake)

| Bytes   | Module                                                | Load-bearing? |
| ------- | ----------------------------------------------------- | ------------- |
| 545,403 | `react-dom` (production cjs)                          | Yes — non-negotiable in a React app. Tree-shakes hard inside esbuild. |
| 179,387 | `src/components/**` (everything we wrote)             | Yes — our app code. |
|  47,398 | `src/styles.css`                                      | Yes — emits `dist/main.css`, inlined into HTML. |
|  31,368 | `src/debugBridge.ts`                                  | Yes — the Chrome MV3 extension contract surface. |
|  18,589 | `react`                                               | Yes — the runtime. |
|  18,126 | `d3-force`                                            | Yes — used by both graph views (split into shared chunk). |
|  11,766 | `d3-quadtree`                                         | Yes — d3-force dependency for n-body sim. |
|  10,375 | `scheduler`                                           | Yes — React 19 dependency (concurrent rendering). |
|   8,653 | `src/App.tsx`                                         | Yes. |
|   8,500 | `src/components/activity/ActivityPanel.tsx`           | Yes. |

## What got removed in this pass

- **`@visx/responsive` + `lodash`** — was 16 KB + 14 KB raw,
  brought in by a single `<ParentSize debounceTime={0}>` usage that
  did **not** actually need debounce. Replaced with a 30-line
  `ParentSize.tsx` using `ResizeObserver` directly. Net: ~30 KB
  raw input dropped, ~5 KB minified saved in the shared lazy chunk.
- The `dist/live.html` no longer carries the JS bundle inline;
  esbuild emits ESM chunks under `/static/<chunk>.js` that the Rust
  crate serves with `Cache-Control: public, max-age=31536000,
  immutable` (chunks are content-hashed by esbuild). HTML stays
  no-store.

## Heaviest non-removable dependencies

`react-dom` dwarfs everything (about 60% of the entry chunk pre-min).
Nothing actionable: replacing React would be a rewrite, and Preact
would lose `useSyncExternalStore` semantics that `useNow.ts` relies
on. d3-force at ~18 KB is the only non-React heavy dep left, and
it's already in a lazy chunk so it never blocks first paint.

## How to refresh this report

```sh
cd crates/crabcc-viz/web
bun run build                                              # writes dist/metafile.json
node -e "$(cat <<'EOF'
const m = JSON.parse(require('fs').readFileSync('dist/metafile.json','utf8'));
const r = {};
for (const [k,v] of Object.entries(m.inputs))
  r[k.match(/^node_modules\/((?:@[^/]+\/)?[^/]+)/)?.[1] ?? k] =
    (r[k.match(/^node_modules\/((?:@[^/]+\/)?[^/]+)/)?.[1] ?? k] ?? 0) + v.bytes;
console.log(Object.entries(r).sort((a,b)=>b[1]-a[1]).slice(0,15));
EOF
)"
```
