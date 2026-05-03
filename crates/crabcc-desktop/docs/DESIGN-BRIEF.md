# `crabcc-desktop` — design brief

> Brief for a designer landing here cold. The Rust app already builds
> and runs against a live local server — most of what you'll redesign
> is **what the user already sees** today, plus a few greenfield
> surfaces marked below. Existing reference: the web dashboard at
> `http://127.0.0.1:7878` while `crabcc serve` is running.
>
> **Source of truth.** This file mirrors GitHub issue
> [#227](https://github.com/peterlodri-sec/crabcc/issues/227) and the
> umbrella initiative [#213](https://github.com/peterlodri-sec/crabcc/issues/213).
> Update this file and the issue together.

## Who this is for

**One developer**, on their own machine, looking at their own work.
Not a team dashboard. Not an enterprise console. The user is staring
at this for hours while they're coding — it lives in a window next
to their editor and terminal. Personal feel matters: it should
reward attention with a sense of **a system that's alive**, not a
report that's printed once.

Concretely: the user is building `crabcc` itself (a Rust CLI + MCP
server for navigating large codebases), and the dashboard lets them
see the codebase, the live agents working in it, the logs, the
memory drawer they keep for themselves, and the tools they can fire
without touching the terminal.

## What this is

A native macOS / Linux desktop app (GPU-rendered via
[`gpui`](https://github.com/zed-industries/zed/tree/main/crates/gpui)
+ [`gpui-component`](https://github.com/longbridge/gpui-component) —
the same UI stack that powers the [Zed editor](https://zed.dev) and
[Longbridge desktop](https://longbridge.com/desktop/)) that watches
the local `crabcc serve` HTTP/SSE server and renders panels of state
plus a launchpad of common actions.

**Native, not web.** The app talks HTTP/SSE to `127.0.0.1:7878` for
data, but the rendering is GPU-direct — text, charts, the relations
graph, animations all paint on the GPU. We're not in a browser.
This unlocks things that would feel wrong on the web: native
window chrome, dock badge, system menu bar, instant local
notifications.

## Vibe

Closer to:
- **A car dashboard** — gauges that move because the engine is moving.
- **Apple's Activity Monitor** + **Zed's live status bar** —
  dense, real, no marketing chrome.
- **A modular synth patch panel** — clearly addressable surfaces,
  cables (data flows) you can almost see.

Avoid:
- Enterprise/SaaS dashboard look (tiles with gradients, big stat
  numbers, cookie banners).
- Game-y / RPG UI (XP bars, achievement pills).
- Pure brutalism (raw text on grey). It needs warmth.

Personal, **dark by default**, **dense by default**, with motion
on the things that are actually moving.

## What's already live (existing surfaces, ready to redesign)

The app today has 7 routes selected from a header nav strip. Every
route observes a shared in-memory `AppState` that 4 background
workers populate. **The data is real** — a designer working on this
runs `cargo run -p crabcc-desktop` against a local `crabcc serve`
and sees their own laptop's state.

### Home (`route: Home`, `routes/dashboard.rs`)
- **KPI strip** (4 cards): index files/symbols count, cumulative
  activity hits since startup, agents running ratio, services
  reachable ratio.
- **Tile row** (3 cards):
  - **Recent activity** — last ~8 query operations (sym, refs,
    callers, outline) with target query + result count, **grouped
    when consecutive operations share a target**, with a
    recency-fade alpha. Updates over SSE.
  - **Agents** — list of recent agents with status dot + runtime.
  - **Services** — local service discovery (Ollama, LiteLLM, OTLP,
    Redis, MCP, etc.) with reachability mark + latency.
- **Relations graph** — full-width canvas, force-directed layout
  of ~96 symbols / ~276 call edges. Static layout (warmup-only,
  no animation today).
- **Inline agent prompt** (`submit_launch`): minimal text field
  posts to `/api/agents/launch`. The richer profile-picker sheet
  is greenfield (see below).

### Logs (`routes/logs.rs`)
- Live tail of structured tracing events (3-second poll).
- Each row: `HH:MM:SS · LEVEL · target · message`.
- **Filter input** + **level pills** (TRACE/DEBUG/INFO/WARN/ERROR)
  + **clickable level badge** in each row (click sets the pill).
- Level colour from theme: TRACE/DEBUG muted, INFO `info`,
  WARN `warning`, ERROR `danger`.

### System (`routes/system.rs`)
- 6 sections (one per data source): service discovery list, OTLP
  collector health pill, Ollama API-key state, agent profile
  registry, agent model registry, recent agent kills.
- Filter input narrows every section in lockstep, with
  `count_line` / `no_match` placeholders.

### Knowledge (`routes/knowledge.rs`)
- Memory drawers (notes the user / agents stick into the local
  `.crabcc/memory.db`). Newest-first list with id, wing/room badge,
  relative timestamp, body preview.
- **Substring filter** + **wing-pin filter** (click a wing badge to
  pin) + **wing distribution summary line**.
- **In-window ingest** field — posts to `/api/memory/ingest` and
  surfaces the new drawer immediately.
- Refreshes every 10s.

### Agents (`routes/agents.rs`)
- Agent list with status dot, runtime, model.
- **Filter input** + **status pills** (any / running / done).
- Click a row → log tail panel below.
- Per-row **Kill** button (POSTs `/api/agents/{id}/kill`).

### Commands (`routes/commands.rs`)
- Categorised launchpad: `CATALOG` is a static list of CLI commands
  grouped by category (index, query, memory, agents, fetch, …),
  rendered with a search input.
- **Greenfield gap**: rows are display-only today. Click-to-run +
  inline result rendering hasn't shipped (see below).

### Graph (`routes/graph.rs`)
- Dedicated route showing the relations graph with **pan**
  (mouse drag, with `DRAG_THRESHOLD_PX`), **zoom** (mouse wheel,
  `MIN_ZOOM`/`MAX_ZOOM`/`SCROLL_K`), **click hit-test**, and
  **neighbour highlight** on selection.
- Header counts: `{nodes} · {edges}` summary.
- **Greenfield gap**: clicking a node highlights neighbours but
  does not yet open a node-detail drawer (see below).

### Native menu-bar / dock (`native.rs`)
- macOS dock badge with running-agent count.
- macOS menu-bar status item with running-agent count.

## What's *not* there yet (greenfield — design from scratch)

These are real product asks; the underlying data / actions exist on
the server but the desktop app doesn't surface them yet. Each item
below maps to a Stitch ref-design screen — see "Stitch ref designs"
appendix.

### 1. Tool-call timeline / inspector
Every time an agent fires a tool (`crabcc.sym`, `crabcc.refs`,
`crabcc.callers`, `crabcc.memory.search`, etc.), the server emits
an event. **Today** these are visible only as activity rows in the
dashboard tile — flat, with simple consecutive-run grouping.
**Want**: a dedicated route or right-rail panel that groups
consecutive calls by the same agent, shows arguments + result
counts inline, and visually distinguishes tools (small icons /
colour codes per tool family). "Pin" a call so it persists after
the buffer rolls.

### 2. Agent-spawn sheet (profile picker + streaming output)
Today a tiny prompt field on Home posts to `/api/agents/launch`
with a hardcoded default profile. **Want**: a sheet that slides
over Home, listing profiles from `/api/agents/profiles` on the
left, prompt textarea on the right, "Launch" button. After
launch, the sheet morphs to a streaming output panel (or hands off
to the Agents route's log tail). 3 states: idle / launching /
streaming.

### 3. Commands launchpad — click-to-run + inline result
The `CATALOG` is rendered already, but rows are static. **Want**:
click a row → POST to its endpoint → show "running…" pulse →
render the result inline below the row, collapsible. Categorised
results (table / list / scalar / error). Keyboard-first selection
with `↑/↓/⏎`.

### 4. Relations graph node-detail drawer
The graph supports pan/zoom/click/neighbour highlight; clicking a
node sets `selected` but doesn't open a panel. **Want**: a slide-in
drawer on the right showing symbol kind, file:line, signature,
incoming + outgoing edges, "Open in editor" + "Show callers"
buttons. The hit-test in `GraphView::handle_click` already returns
the node id — the drawer is the missing UX.

### 5. Knowledge graph canvas
Memory drawers cross-reference each other through wings/rooms. The
server already computes the relationships; we don't render them.
**Want**: a second graph visualisation, distinct from the
relations graph (cooler palette, semantically different —
relations = code, knowledge = thinking).

### 6. In-window notifications strip (precedes rich macOS notifications)
Track C of #213 ships rich macOS system notifications via `objc2`
+ a Swift `.appex`. Before that lands, **want**: an in-window
strip at the top of every route showing the last 3-5 notifications
(with stacking + auto-dismiss). Doubles as a fallback when the
system notification surface is unavailable (Linux, sandbox issues,
etc.).

## Live data — what's actually moving

This matters for the "lively feeling". Tell the designer where
animation is justified vs. distracting:

| Surface | Update cadence | Animate? |
|---|---|---|
| Activity row insert | every few seconds (SSE) | Yes — slide-in / brief highlight + alpha fade by age (`FADE_FRESH_SECS` / `FADE_STALE_SECS`) |
| Agents list | every few seconds (SSE) | Status dot only (live pulse for "running") |
| Telemetry log row | 3s polled | Yes — gentle fade-in on new rows |
| Memory drawers | 10s polled | Yes — soft pulse on the new drawer |
| KPI numbers | continuous | Number tween (no flicker) |
| Services reachability | static after prefetch | No — but a "last probed Ns ago" tag is fine |
| Relations graph | static today | No (until the drawer interactivity lands) |

Don't animate everything. **Motion = "this just changed".**
Static surfaces shouldn't move ambient — that creates anxiety.

## Interaction patterns the app should encourage

- **Discoverability over memorisation.** A power user should still
  feel comfortable, but the app shouldn't require knowing CLI flags.
- **Inline results.** When you fire a command, the result lands
  next to where you fired it — not in a different panel.
- **Permission-free actions.** No confirmation dialogs for safe
  ops. Confirm only for destructive ones (kill running agent, drop
  drawer).
- **Keyboard-first where it makes sense** — but don't punish the
  mouse-only user. The CLI exists for the keyboard die-hards.
- **Native window chrome.** Use macOS/Linux native title bar
  conventions, not custom-drawn web-style headers.

## Technical constraints (read these before sketching)

- **GPU-rendered**, not HTML/CSS. The toolkit is gpui; layout
  primitives are flexbox-like (`v_flex` / `h_flex` / `gap_*` /
  `px_*`), and styling uses theme tokens (`background`, `foreground`,
  `border`, `secondary`, `muted_foreground`, `primary`, `info`,
  `warning`, `success`, `danger`, `chart_1` … `chart_5`). The
  activity tile already maps tool families to tokens
  (`routes/dashboard.rs:475`):

  | Tool family | Token |
  |---|---|
  | `sym`, `ingest`/`memory.ingest` | `primary` |
  | `refs` | `info` |
  | `callers` | `warning` |
  | `fuzzy`, `prefix`, `random-query` | `success` |

- **No `card` / shadcn-style elevation tokens.** Today we use
  `secondary` for elevated surfaces. If you want clearer elevation,
  call it out — we'll borrow from the upstream component library.
- **Custom SVG icons need to be packaged with the app**; we're not
  loading Lucide over the network. We can ship an icon set, but it
  has to be specified.
- **Charts** ship with gpui-component (`LineChart`, `AreaChart`,
  `PieChart`) — palette via `theme.chart_1`…`chart_5`.
- **Animation budget**: motion at 60fps is free; animating 1000+
  elements simultaneously is not. The relations graph caps around
  500 nodes today.
- **Cross-platform**: macOS first, Linux is supported. Windows is
  out of scope for the first version.

## What the designer's deliverable should look like

In rough order of value:
1. **Mood board** — references the project should feel like
   (apps, screenshots, palettes, typefaces).
2. **Wireframes** for the existing routes + the 6 greenfield
   surfaces above. Annotate which elements are live (data-driven),
   which are static.
3. **Component spec** — buttons, inputs, badges, pills, list rows,
   tile cards, graph nodes/edges. With states (default / hover /
   active / disabled / error).
4. **Theme spec** — colours (dark default + light variant), spacing
   scale, type ramp, radius / elevation tokens. Map to gpui-component
   theme fields where 1:1, call out gaps.
5. **One animated GIF / video** of the home dashboard at idle,
   demonstrating the "lively but not jittery" balance.
6. **Iconography** — 12–20 icons covering the tool families
   (sym / refs / callers / outline / fuzzy / memory / fetch / agent /
   index / serve / mcp). SVGs.

## Out of scope

- **Mobile / tablet layouts.** This is desktop, single-window.
- **Marketing pages / onboarding.** No login, no account.
- **Theming for end-users.** One curated dark + one curated light
  is enough for the first version.
- **Internationalisation.** English only at v1.
- **Remote / multi-machine.** Loopback only.

## Open design questions

1. **Information density** — closer to Activity Monitor (very dense)
   or Zed (medium)? Pick one and stick to it; mixing reads as
   inconsistent.
2. **Sidebar vs. top nav** — currently top nav. A sidebar buys us
   space for the launchpad / agent shortcuts but eats horizontal
   width on smaller windows.
3. **One-click agent spawn — sheet vs. inline panel?** Sheet is
   modal-ish but easier to dismiss; inline panel is more "keep
   working" but takes screen space. The Stitch mockup proposes a
   sheet (#2 above) — overrideable.
4. **Graph + knowledge — same canvas with toggle, or two distinct
   canvases?** Both have pros; the Stitch mockup proposes two
   distinct canvases (#5 above).
5. **Notifications surface** — do we render a "last 5 notifications"
   strip somewhere visible (#6 above), or rely on the OS notification
   centre? The brief recommends the in-window strip for the v1 of
   Track C.
6. **Apple Developer Team ID** — gates Track C.3 (App Groups for the
   `.appex` content extension). Out of scope for the desktop UI
   work, but blocks the rich-notification finish line.
7. **Tailwind for Track B (web refresh)** — adopt for B.1 or stay on
   plain CSS variables? Affects bundle size and dev velocity.
   Decision needed before any B.x mockup is meaningful.

## References worth opening

- The web dashboard (`http://127.0.0.1:7878` while `crabcc serve`
  is running) — this is what users see *today* on the web side.
- `crates/crabcc-viz/web/src/components/` — the React side of the
  same surfaces, often with more visual polish than the gpui port.
- [Longbridge desktop](https://longbridge.com/desktop/) — visual
  benchmark for what a gpui-component desktop app can look like.
- [Zed](https://zed.dev) — gpui itself, slightly different problem
  domain but the same toolkit.
- [shadcn/ui](https://ui.shadcn.com/docs) — the design system the
  React side is migrating toward (Track B); useful as a vocabulary
  baseline.
- [`./RESEARCH-native-desktop-and-rich-notifications.md`](./RESEARCH-native-desktop-and-rich-notifications.md)
  for the technical phasing.
- [`./RESEARCH-apple-rich-notifications-dossier.md`](./RESEARCH-apple-rich-notifications-dossier.md)
  for the Apple-side rich notification dossier (Track C).

---

## Appendix A — current state (auto-updated alongside route work)

| Brief surface | State | Source |
|---|---|---|
| Home (KPI + tiles + activity grouping + recency fade) | shipped | `routes/dashboard.rs` |
| Logs (filter + level pills + clickable level) | shipped | `routes/logs.rs` |
| System (services / OTLP / Ollama / profiles / models / kills + filter) | shipped | `routes/system.rs` |
| Knowledge (drawers + filter + wing pin + in-window ingest + wing summary) | shipped | `routes/knowledge.rs` |
| Agents route (list + filter + status pills + log tail + kill) | shipped | `routes/agents.rs` |
| Commands launchpad (search + categorised list) | partial — display only | `routes/commands.rs` |
| Relations graph (pan / zoom / click / neighbour highlight) | shipped | `routes/graph.rs` |
| Relations graph node-detail drawer | greenfield | — |
| Agent-spawn sheet | partial — inline prompt only | `routes/dashboard.rs` |
| Tool-call timeline / inspector | greenfield | — |
| Knowledge graph canvas | greenfield | — |
| In-window notifications strip | greenfield | — |
| Rich macOS notifications (Track C.2–C.5) | greenfield | `native.rs` (only C.1 dock + menu-bar landed) |
| Shadcn-flavoured web refresh (Track B) | not started | `crates/crabcc-viz/web/` |

## Appendix B — Stitch ref designs

A Stitch project (`projects/3825002640704158815` —
**`crabcc-desktop greenfield`**) was opened with a dark design system
mirroring the gpui-component tokens above. Six screen prompts were
submitted (one per greenfield surface). At the time of writing, the
Stitch screen-generation API was returning timeouts on every call
and `list_screens` stayed empty — so this appendix ships **without**
embedded URLs. The Stitch project + design system survive and can
be re-driven later via `mcp__stitch__generate_screen_from_text` once
the API stabilises; the prompts that should be replayed live in
`crates/crabcc-desktop/docs/DESIGN-PROMPTS.md`.

The brief itself (above) is the actual spec — every greenfield
surface has its target shape, states, copy, density, and theme
tokens defined inline. Implementation does not strictly need the
Stitch screens to start, but the screens (when they land) should be
backfilled here:

| # | Surface | Stitch screen | Follow-up issue |
|---|---|---|---|
| 1 | Tool-call timeline / inspector | _pending_ | [#293](https://github.com/peterlodri-sec/crabcc/issues/293) |
| 2 | Agent-spawn sheet (3 states) | _pending_ | [#294](https://github.com/peterlodri-sec/crabcc/issues/294) |
| 3 | Commands launchpad (run + result) | _pending_ | [#295](https://github.com/peterlodri-sec/crabcc/issues/295) |
| 4 | Relations graph node-detail drawer | _pending_ | [#296](https://github.com/peterlodri-sec/crabcc/issues/296) |
| 5 | Knowledge graph canvas | _pending_ | [#297](https://github.com/peterlodri-sec/crabcc/issues/297) |
| 6 | In-window notifications strip | _pending_ | [#298](https://github.com/peterlodri-sec/crabcc/issues/298) |

Stitch project: `projects/3825002640704158815` (`crabcc-desktop greenfield`).
Design system: `assets/9253917256800618914` (`crabcc-desktop dark`).
