# crabcc-desktop — Architecture

GPUI-rendered native dashboard for `crabcc serve`. This document
captures the data-flow shape that holds the crate together — what
runs on which thread, who feeds whom, where the state-update
ownership boundaries are.

## Top-level layout

```
┌────────────────────────────────────────────────────────────────────────────┐
│  main()                                                                    │
│   ├─ tracing-subscriber init                                               │
│   ├─ services::ensure_stack_started() ─▶ (BootstrapOutcome, Duration)      │
│   ├─ ctrlc handler (opt-in via CRABCC_DESKTOP_STOP_SERVICES_ON_EXIT)       │
│   └─ gpui_platform::application().run(closure)                             │
│        │                                                                   │
│        ▼                                                                   │
│       cx.spawn(async move |cx|                                             │
│         cx.open_window(options, |window, cx|                               │
│           let app_state = cx.new(|cx| state::build(...))                   │
│           push_toast(bootstrap_toast)  ──┐                                 │
│           let shell = cx.new(|cx|        │                                 │
│             Shell::new(app_state, ..))   │                                 │
│           cx.new(|cx| Root::new(shell))  │                                 │
│         )                                │                                 │
│       )                                  │                                 │
└──────────────────────────────────────────┼─────────────────────────────────┘
                                           │
                                           ▼
                          ┌────────────────────────────────────┐
                          │  AppState (Entity<AppState>)       │
                          │   ┌──────────────────────────────┐ │
                          │   │ bootstrap, services, graph,  │ │
                          │   │ memory_recent, agents (Vec), │ │
                          │   │ recent_activity (VecDeque),  │ │
                          │   │ telemetry (VecDeque),        │ │
                          │   │ toasts (VecDeque),           │ │
                          │   │ toast_history (VecDeque),    │ │
                          │   │ toasts_muted, echo_to_system │ │
                          │   └──────────────────────────────┘ │
                          │   ┌──────────────────────────────┐ │
                          │   │ apply(event: AppEvent)       │ │
                          │   │   pure mutation              │ │
                          │   │ push_toast / dismiss_toast / │ │
                          │   │   gc_expired_toasts /        │ │
                          │   │   clear_visible_toasts /     │ │
                          │   │   clear_toast_history /      │ │
                          │   │   toggle_toast_mute /        │ │
                          │   │   toggle_echo_to_system      │ │
                          │   └──────────────────────────────┘ │
                          └─────┬──────────────────────────────┘
                                │ observed
                                ▼
                          ┌──────────────────────────────────────┐
                          │  Shell view (Entity<Shell>)          │
                          │   header + nav + body slot           │
                          │   owns:                              │
                          │     • route entities (Home, Agents,  │
                          │       Logs, System, Knowledge,       │
                          │       Commands, K-Graph, Timeline)   │
                          │     • toast strip (ToastStrip)       │
                          │     • native sentinels:              │
                          │         last_badge_count             │
                          │         last_status_count            │
                          │         last_delivered_toast_id      │
                          └──┬───────────────────────────────────┘
                             │ render() — also fires native side-effects
                             ▼
                  ┌─────────────────────────────────┐
                  │  native::* (macOS only)         │
                  │   set_dock_badge                │
                  │   set_status_item               │
                  │   deliver_notification          │
                  │     (osascript shell-out)       │
                  └─────────────────────────────────┘
```

## Channels + worker threads

`state::spawn_workers` multiplexes four background workers through
a single `flume::bounded(512)` channel feeding `AppState::apply`:

```
┌─────────────────────┐     ┌────────────────────────────────────┐
│ prefetch_worker     │ ──► │  AppEvent::Initial(Box<Prefetch>)  │
│   one-shot at start │     │    9 sub-results                   │
└─────────────────────┘     └────────────────────────────────────┘
┌─────────────────────┐     ┌────────────────────────────────────┐
│ sse::spawn_worker   │ ──► │  AppEvent::Sse(SseEvent)           │
│   long-lived stream │     │    Activity / Agents / Unknown     │
│   bounded(256)      │     │  (frame_size capped, drop newest)  │
└─────────────────────┘     └────────────────────────────────────┘
┌─────────────────────┐     ┌────────────────────────────────────┐
│ telemetry_worker    │ ──► │  AppEvent::Telemetry(Result<…>)    │
│   3s poll tick      │     │    edge-trigger Warning/Recovery   │
└─────────────────────┘     └────────────────────────────────────┘
┌─────────────────────┐     ┌────────────────────────────────────┐
│ memory_worker       │ ──► │  AppEvent::MemoryRefresh(…)        │
│   10s poll tick     │     │    edge-trigger Warning/Recovery   │
└─────────────────────┘     └────────────────────────────────────┘
                                            │
                                            ▼
                            ┌────────────────────────────────┐
                            │ pump_events (gpui main thread) │
                            │  rx.recv_async().await         │
                            │   ▼                            │
                            │  app_state.update(cx, |s, cx|  │
                            │     s.apply(ev); cx.notify();  │
                            │  )                             │
                            └────────────────────────────────┘
```

Plus four UI-driven submit paths spawn detached `std::thread`s and
post results back through the same channel:

| Submit       | Posts back as                      |
|--------------|------------------------------------|
| `submit_ingest`     | `AppEvent::MemoryIngestResult` + follow-up `MemoryRefresh` |
| `submit_launch`     | `AppEvent::AgentLaunchResult`      |
| `submit_kill`       | `AppEvent::AgentKillResult`        |
| `submit_agent_log`  | `AppEvent::AgentLogResult { id }`  |
| `submit_command_run`| `AppEvent::CommandRunResult`       |

Both channels are bounded (256 / 512 cap, drop-newest with a
warn-log on overflow). Memory growth is provably bounded by
`cap × event size`.

## Toast lifecycle (track C.0 + C.2)

```
caller pushes toast
   │
   ▼
AppState::push_toast(level, msg)
   │
   │   1. assigns id (monotonic, never reused)
   │   2. logs to toast_history (cap 50, even when muted)
   │   3. if !toasts_muted: gc_expired_toasts(); enqueue at front
   │      (cap 5, evicts oldest)
   │
   ▼
ToastStrip::render — observes AppState
   │
   │   • visible = state.toasts.iter().filter(is_active(now))
   │   • per-row: glyph + message + (optional ↗ system tag) + ×
   │   • footer: [dismiss all] · history (N) · expand · clear
   │
   ▼
Shell::render — also observes AppState
   │
   │   for toast in state.toasts where id > last_delivered_toast_id:
   │     if echo_to_system { native::deliver_notification(...) }
   │     last_delivered_toast_id = newest.id  (advances even when echo off)
   │
   ▼
osascript -e 'display notification "..."'  (macOS only; non-macOS no-op)
```

User controls in the header:
- `● alerts` — mute everything (clears visible, suppresses pushes,
  history still records).
- `↗ system` — keep in-window, suppress system delivery only.

Edge-trigger emits keep failure-recovery cycles to one
toast-per-window via `telemetry_warning_id` / `memory_warning_id`
sentinels.

## Services lifecycle

```
                 ┌──────────────────────────────┐
                 │ services::ensure_stack_started│ ── on app launch
                 └─┬────────────────────────────┘
                   │
                   ├─ env CRABCC_DESKTOP_SKIP_SERVICES set?
                   │    └─▶ SkippedByEnv (no docker action)
                   │
                   ├─ probe /api/health (5s timeout)?
                   │    └─▶ AlreadyRunning (silent happy path)
                   │
                   ├─ docker info reachable?
                   │    └─▶ no? DockerUnavailable (Danger toast)
                   │
                   ├─ docker compose -f install/dev/docker-compose.yml up -d
                   │    └─▶ failed? ComposeFailed { stderr } (Danger toast)
                   │
                   └─ wait_for_health (deadline 30s, 0.5s sleep)
                        ├─▶ Ok: StartedViaCompose (Success "in 2.3s")
                        └─▶ timeout: StartedButNotReady (Danger)

                 ┌──────────────────────────────┐
                 │ services::stop_stack         │ ── opt-in, SIGINT path
                 └─┬────────────────────────────┘
                   │  (only if CRABCC_DESKTOP_STOP_SERVICES_ON_EXIT=1)
                   ▼
                 docker compose down
```

## Why standalone (recap of the README)

`crabcc-desktop` is excluded from the parent Cargo workspace.
`gpui-component` pulls a hard `tree-sitter = "0.25"` with `links =
"tree-sitter"`, but `crabcc-core` is on `tree-sitter = "0.22"`.
Joining the workspace would force a six-grammar coordinated bump.

Practical consequence: this crate has its own `Cargo.lock`, runs
its own dedicated CI workflow, and `cargo` commands operate from
inside its directory.

## Pointers

- Source tree map: see top-level `README.md` "Module map".
- Design intent / Stitch screen refs: `DESIGN-BRIEF.md` in this
  directory.
- Apple-side native research (UN center, app groups, APNs):
  `RESEARCH-apple-rich-notifications-dossier.md`.
- Track-level umbrella architecture for the kickoff initiative:
  `RESEARCH-native-desktop-and-rich-notifications.md`.
