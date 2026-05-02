# `crabcc-desktop` — design brief

> Brief for a designer landing here cold. The Rust app already builds
> and runs against a live local server — most of what you'll redesign
> is **what the user already sees** today, plus a few greenfield
> surfaces marked below. Existing reference: the web dashboard at
> `http://127.0.0.1:7878` while `crabcc serve` is running.

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
the local `crabcc serve` HTTP/SSE server and renders four panels of
state plus a launchpad of common actions.

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

The app today has 4 routes selected from a header nav strip. Every
route observes a shared in-memory `AppState` that 4 background
workers populate. **The data is real** — a designer working on this
runs `cargo run -p crabcc-desktop` against a local `crabcc serve`
and sees their own laptop's state.

### Home (`route: Home`)
- **KPI strip** (4 cards): index files/symbols count, cumulative
  activity hits since startup, agents running ratio, services
  reachable ratio.
- **Tile row** (3 cards):
  - **Recent activity** — last ~8 query operations (sym, refs,
    callers, outline) with target query + result count. Updates
    over SSE.
  - **Agents** — list of recent agents with status dot + runtime.
  - **Services** — local service discovery (Ollama, LiteLLM, OTLP,
    Redis, MCP, etc.) with reachability mark + latency.
- **Relations graph** — full-width canvas, force-directed layout
  of ~96 symbols / ~276 call edges. Static layout (warmup-only,
  no animation today).

### Logs (`route: Logs`)
- Live tail of structured tracing events (3-second poll).
- Each row: `HH:MM:SS · LEVEL · target · message`.
- Level colour from theme: TRACE/DEBUG muted, INFO info, WARN
  warning, ERROR danger.

### System (`route: System`)
- 6 sections (one per data source): service discovery list, OTLP
  collector health pill, Ollama API-key state, agent profile
  registry, agent model registry, recent agent kills.

### Knowledge (`route: Knowledge`)
- Memory drawers ("notes" the user / agents stick into the local
  `.crabcc/memory.db`). Newest-first list with id, wing/room badge,
  relative timestamp, body preview.
- Refreshes every 10s.

## What's *not* there yet (greenfield — design from scratch)

These are real product asks; the underlying data / actions exist on
the server but the desktop app doesn't surface them.

### Tool-call timeline / inspector
Every time an agent fires a tool (`crabcc.sym`, `crabcc.refs`,
`crabcc.callers`, `crabcc.memory.search`, etc.), the server emits
an event. **Today** these are visible only as activity rows — flat,
unstructured. **Want**: a richer timeline that groups consecutive
calls by the same agent, shows arguments + result counts inline,
and visually distinguishes tools (small icons / colour codes per
tool family).

### Available commands launchpad
The CLI has dozens of subcommands (`crabcc index`, `crabcc fetch`,
`crabcc memory ingest`, `crabcc agents run …`, `crabcc fuzzy`, …).
Most can be triggered over the HTTP API. **Want**: a discoverable
launchpad — search-as-you-type, categorised, click to run.
Immediate visual feedback ("running…" → result inline). Should
feel less like a menu and more like a console an experienced user
would actually use.

### One-click agent spawn
Today the user runs an agent by typing
`crabcc agents run --profile <id> --prompt "…"` in the terminal.
**Want**: from inside the dashboard, click an agent profile from
the System route's profile list → opens a tiny sheet asking only
for the prompt → spawns it → live status / streaming output flows
into a panel in the same window. The wire surface exists
(`POST /api/agents/launch`); the UI doesn't.

### In-window memory ingest
Currently you can only add memory drawers via
`crabcc memory ingest` in the terminal. **Want**: a small text
field somewhere on the Knowledge route that posts to
`/api/memory/ingest` and immediately surfaces the new drawer.

### Relations graph interactivity
The graph today is a still photograph. **Want**: zoom (mouse
wheel), pan (mouse drag), click a node → drawer slides in from
the right with the node's incoming/outgoing edges + a button to
"open in editor". Hover state for nearby nodes. Maybe always-on
labels for the top-N hub nodes.

### Knowledge graph (separate from relations graph)
Memory drawers cross-reference each other. The server already
computes a graph; we don't render it. **Want**: a second graph
visualisation, distinct from the relations graph (different
colour palette, semantically different — relations = code,
knowledge = thinking).

## Live data — what's actually moving

This matters for the "lively feeling". Tell the designer where
animation is justified vs. distracting:

| Surface | Update cadence | Animate? |
|---|---|---|
| Activity row insert | every few seconds (SSE) | Yes — slide-in / brief highlight |
| Agents list | every few seconds (SSE) | Status dot only (live pulse for "running") |
| Telemetry log row | 3s polled | Yes — gentle fade-in on new rows |
| Memory drawers | 10s polled | Yes — soft pulse on the new drawer |
| KPI numbers | continuous | Number tween (no flicker) |
| Services reachability | static after prefetch | No — but a "last probed Ns ago" tag is fine |
| Relations graph | static today | No (until interactivity lands) |

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
  `warning`, `success`, `danger`, `chart_1` … `chart_5`).
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
2. **Wireframes** for the 4 existing routes + 5 greenfield surfaces
   above. Annotate which elements are live (data-driven), which
   are static.
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
   working" but takes screen space.
4. **Graph + knowledge — same canvas with toggle, or two distinct
   canvases?** Both have pros; needs a sketch.
5. **Notifications surface** — do we render a "last 5 notifications"
   strip somewhere visible, or rely on the OS notification centre?

## References worth opening

- The web dashboard (`http://127.0.0.1:7878` while `crabcc serve`
  is running) — this is what users see *today*.
- `crates/crabcc-viz/web/src/components/` — the React side of the
  same surfaces, often with more visual polish than the gpui port.
- [Longbridge desktop](https://longbridge.com/desktop/) — visual
  benchmark for what a gpui-component desktop app can look like.
- [Zed](https://zed.dev) — gpui itself, slightly different problem
  domain but the same toolkit.
- [shadcn/ui](https://ui.shadcn.com/docs) — the design system the
  React side is migrating toward; useful as a vocabulary baseline.
- The kickoff PR's research dossier:
  [`./RESEARCH-native-desktop-and-rich-notifications.md`](./RESEARCH-native-desktop-and-rich-notifications.md)
  for the technical phasing.
