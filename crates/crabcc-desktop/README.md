# crabcc-desktop

GPUI-rendered native dashboard for `crabcc serve`. Six routes
(Home / Agents / Logs / System / Knowledge / Commands), live SSE
bridge, force-directed relations graph, and a native macOS surface
(dock badge + menu-bar status item).

> **Standalone crate.** Not a workspace member — see [Why standalone](#why-standalone) below.

## Quick start

```bash
# Single terminal — the binary auto-starts the docker-compose
# backend stack on launch (see `src/services.rs`). If the stack is
# already up or `crabcc serve` is running natively, the auto-start
# is a no-op.
cd crates/crabcc-desktop
cargo run --release
```

A 1600×1000 window opens with the live dashboard. The header nav
switches between the six routes; AppState observers re-render on
every SSE frame / poll tick. If the stack needed starting, the
toast strip pops a Success "backend started via docker compose"
banner once `/api/health` answers.

Set `CRABCC_DESKTOP_SKIP_SERVICES=1` to opt out of the auto-start
(useful when `crabcc serve` is already running from another shell
or when iterating on the binary in CI).

If you have [Task](https://taskfile.dev) installed
(`brew install go-task`):

```bash
task                 # default — build + lint + test (the daily gate)
task run             # debug-mode launch
task run-rel         # release-mode launch (recommended)
task services-up     # start backend stack manually (mirrors auto-start)
task services-down   # stop backend stack (graceful counterpart)
task services-status # docker compose ps
task services-logs   # tail backend logs (SVC=<svc> to scope)
task bench           # criterion benches
task watch           # cargo-watch + auto-reload
task --list          # full menu
```

## Routes (what's where)

```
┌─ titlebar ──────────────────────────────────────────────────────────┐
│  [Home] [Agents] [Logs] [System] [Knowledge] [Commands]             │
├─────────────────────────────────────────────────────────────────────┤
│  Home       KPI strip · activity / agents / services tile row       │
│             Agent-spawn form · force-directed relations graph       │
│             Click op badge to pin op-filter on activity tile        │
├─────────────────────────────────────────────────────────────────────┤
│  Agents     One card per agent: id / runtime / model / pid / age    │
│             Substring filter + status pills + Kill button           │
│             Click card to expand log tail; Refresh re-fetches       │
├─────────────────────────────────────────────────────────────────────┤
│  Logs       Telemetry tail (3s poll) · level pills · click level    │
│             badge in row to drill in · context-aware empty-state    │
├─────────────────────────────────────────────────────────────────────┤
│  System     Services + OTLP + Ollama + profiles + models + kills    │
│             Single filter input narrows every long section          │
├─────────────────────────────────────────────────────────────────────┤
│  Knowledge  Memory drawer browser (10s poll) · ingest form          │
│             Substring filter · click wing badge to pin filter       │
│             Wing distribution summary line                          │
├─────────────────────────────────────────────────────────────────────┤
│  Commands   Searchable static catalog of the crabcc CLI surface     │
└─────────────────────────────────────────────────────────────────────┘
```

## Module map

| Module | Role |
|---|---|
| `api/`        | Typed HTTP client + wire types (mirrors `crates/crabcc-viz/web/src/api.ts`). |
| `sse`         | Long-lived SSE worker, smol-friendly via `flume`. |
| `state`       | `AppState` entity, `AppEvent` union, four-worker bridge, `Route` enum. |
| `routes/`     | Body content per route. One file per route. |
| `graph_layout`| Pure-compute force-directed layout for the relations graph. |
| `shell`       | Top-level header + nav + body switcher. Owns native side-effect surfaces (dock badge, status item). |
| `native`      | macOS-only AppKit hooks (dock badge, NSStatusItem). Compile-time no-op stubs on other platforms. |

## Native macOS surfaces

| Surface | Trigger | Implementation |
|---|---|---|
| Dock-tile badge | `running-agents != 0` | `NSDockTile.setBadgeLabel:` (objc2) |
| Menu-bar status item | `running-agents != 0` | `NSStatusItem` with `setVisible:` toggle (objc2; cached `Retained<…>` in a thread-local OnceCell) |

Both surfaces share the running-agents count today but track two
independent change-detection sentinels on `Shell` so they can be
re-purposed individually later. Non-macOS builds compile the
surfaces as no-op stubs.

Future native work (Track C, not yet shipped):

- `UNNotificationCategory` + actions + delegate (rich notifications).
- App Group + entitlements + first `.appex` (Apple Dev Team gated).
- APNs path (remote rich pushes).

## Background workers + state plumbing

A single `flume` channel multiplexes four workers feeding `AppState`:

| Worker | Cadence | Surface |
|---|---|---|
| `prefetch_worker` | one-shot | bootstrap / services / seed-graph / memory_recent / otlp_health / agent_profiles / agent_kills / agent_models / ollama_key |
| `sse::spawn_worker` | long-lived stream | activity / agents (via `/api/events`) |
| `telemetry_worker` | 3s poll | `/api/telemetry?since=<cursor>` |
| `memory_worker` | 10s poll | `/api/memory/recent` |

Plus four UI-driven submit paths (`submit_ingest` / `submit_launch`
/ `submit_kill` / `submit_agent_log`) that spawn detached
`std::thread`s and post results back through the same channel.

Both channels are **bounded**:

| Channel | Cap | Overflow |
|---|---:|---|
| `sse::spawn_worker` | 256 | drop newest + warn-log |
| `state::spawn_workers` | 512 | drop newest + warn-log |

Memory growth is provably bounded by `cap × event size`.

## Performance baselines

`cargo bench` (criterion) under `benches/`:

| Bench | Time (M-series macOS) |
|---|---:|
| `apply_agents_frame_50` | ~13.5 µs |
| `apply_activity_burst_100` | ~0.96 µs |
| `apply_activity_drip_100x1` | ~1.92 µs |
| `graph_layout_50_nodes` | (depends on machine) |
| `graph_layout_500_nodes` | (depends on machine) |

The numbers above represent the post-`SharedString`-flip steady
state. The `apply_agents_frame_50` cost includes one-time
per-agent `format!()` for cached gpui ElementIds (decode-time, not
per-render — see `AgentDerived` in `api/types.rs`).

## Why standalone

`crabcc-desktop` is **not** a member of the parent Cargo workspace.

`gpui-component` pulls a hard `tree-sitter = "0.25"` with `links =
"tree-sitter"`, but `crabcc-core` is on `tree-sitter = "0.22"` with
the grammar fleet at `0.21`. Joining the workspace would force a
six-grammar coordinated bump. Standalone keeps the gpui ecosystem
moving at its own cadence.

Practical consequence: `cargo` commands run from `crates/crabcc-desktop`
operate on this crate's own `Cargo.lock` independent of the workspace.

## gpui pin strategy

```toml
gpui           = { git = "https://github.com/zed-industries/zed" }
gpui_platform  = { git = "https://github.com/zed-industries/zed", features = [...] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "..." }
```

- `gpui` and `gpui_platform` track gpui-component's own source URLs
  **without a rev pin** — cargo unifies the gpui crate to a SINGLE
  compiled copy.
- Pinning a `rev =` on the top-level `gpui` line splits the
  resolution from gpui-component's revless source URL and produces
  two zed checkouts + a `Render` trait collision at compile time.
- Reproducibility comes from `Cargo.lock` instead.
- Only `gpui-component` is pinned by rev (it's published from one
  place and we want a deterministic upstream surface).

To bump gpui-component:

1. `cd crates/crabcc-desktop && cargo update -p gpui-component --precise <new-rev>`
2. `cargo run` — verify the window still renders.
3. Commit the lockfile change.

To bump `gpui` itself: `cargo update -p gpui` — the unification
across `gpui` / `gpui_platform` / `gpui-component`'s transitive zed
dep happens automatically because all three reference the same
revless source URL.

## Filter pattern (cross-route)

Every data-heavy route uses the same substring-filter shape:

```rust
pub struct SomeRoute {
    state: Entity<AppState>,
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input's value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing per render.
    query_lower: String,
}
```

UI affordance lives on the route entity, not on `AppState`.
Header switches between "N total" (no filter) and "X of N match"
(filter active). Distinct empty-state copy when a filter
mismatches everything ("no <noun> match X").

Routes layer on top of this shape:

- **Agents** — status pills (All / Running / Exited).
- **Logs** — level pills (TRACE / DEBUG / INFO / WARN / ERROR);
  click any row's level badge to drill in.
- **Knowledge** — wing-pin (click any drawer's wing badge);
  wing distribution summary line.
- **Home activity tile** — op-pin (click any op badge).

## Development

```bash
cd crates/crabcc-desktop

# Daily gate.
task                       # build + lint + fmt-check + test

# Faster iteration.
task watch                 # cargo-watch + auto-reload

# Bench harness (criterion).
task bench                 # full run, ~30 s
task bench-quick           # --quick, ~3 s

# Code quality.
task lint                  # clippy --all-targets -- -D warnings
task fmt                   # cargo fmt
task fmt-check             # cargo fmt -- --check
```

The bench harness is gated by `[dev-dependencies] criterion`. Bench
HTML reports land under `target/criterion/`. Diff between two runs
via `cargo bench -- --save-baseline X` then
`cargo bench -- --baseline X`.

## In-window notifications + native banners

Track C.0 + C.2 surfaces — see also "Routes" diagram above.

| Surface | What | Where |
|---|---|---|
| Toast strip | 5 levels (success / info / warning / danger / primary) stacked under the header. Click `×` to dismiss; auto-dismiss intervals: success 5s, info 3s, warning + danger persist, primary 8s. | `src/toasts.rs` |
| Auto-emit | Submit results (ingest / launch / kill × ok / err), prefetch errors, telemetry + memory poll edge-triggers (one warning per failure window, success on recovery), agent Running → Exited transitions. | `state::AppState::apply` |
| History | Append-only audit log of every push including muted (cap 50). Footer `[dismiss all] · history (N) · expand · clear` toggles inline expanded view. | `src/toasts.rs` |
| Mute | Header `● alerts` button. Mute clears the visible deque, suppresses new pushes (history still records). | `state::toggle_toast_mute` |
| System echo | Header `↗ system` button. When on, every visible toast also fires a banner via Notification Center. When off, in-window only. | `state::toggle_echo_to_system` + `Shell::render` |
| macOS banner | `osascript -e 'display notification ...'` from `native::deliver_notification`. Sidesteps the `.app` bundle requirement of `UNUserNotificationCenter`; future C.2.1 ships a real bundle. | `src/native.rs` |

## Backend stack lifecycle

`crabcc-desktop` auto-starts the docker-compose stack on launch
(see `src/services.rs`). On a fresh checkout the binary alone is
enough to bring up `crabcc serve` + dependent containers.

| Phase | Behaviour |
|---|---|
| Auto-start | `services::ensure_stack_started` — fast-path probe `/api/health` → if down, `docker info` → `docker compose -f install/dev/docker-compose.yml up -d` → wait for health (30s deadline). Surfaces outcome on the toast strip. |
| Skip | `CRABCC_DESKTOP_SKIP_SERVICES=1` — bypass entirely (devs running `crabcc serve` from another shell). |
| Stop on exit | `CRABCC_DESKTOP_STOP_SERVICES_ON_EXIT=1` — opt-in SIGINT handler runs `docker compose down` on Ctrl-C. Off by default since most users have other consumers of the stack. |
| Manual ops | `task services-up` / `services-down` / `services-status` / `services-logs`. |

## Architecture

Full data-flow chart — workers, channels, toast lifecycle,
services lifecycle — lives in the **private** companion repo at
[`docs/desktop/ARCHITECTURE.md`](https://github.com/peterlodri-sec/crabcc-docs/blob/main/desktop/ARCHITECTURE.md)
(checked out as a git submodule of the parent repo at `docs/`).

## Tracking

| Phase | Status |
|---|---|
| Track A — desktop dashboard | feature-complete (#214) |
| Track B — Tailwind / shadcn | not started (B.1+) |
| Track C.0 — in-window toast strip | shipped (slices 1-6 + dismiss-all + system tag + echo toggle) |
| Track C.1 / C.1.1 — dock badge + status item | shipped |
| Track C.2 — macOS rich notifications | first wedge shipped (osascript). C.2.1 bundles as `.app` for `UNUserNotificationCenter` + actions; not started |
| Track C.3+ — App Group + entitlements + APNs | Apple-Dev-gated; not started |

Living roadmap: PR [#214](https://github.com/peterlodri-sec/crabcc/pull/214).

## License

Same as the parent repository (MIT).
