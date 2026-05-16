# DESIGN.md — Engineering overview of the crabcc desktop UI

> One file, no fluff: how the desktop dashboard is wired and how to keep
> new surfaces consistent with what's there. For *what* the dashboard
> shows (per-route brief), see `crates/crabcc-desktop/docs/DESIGN-BRIEF.md`.
> This doc is the *how*.

---

## 1. Crate-level shape

```
crates/crabcc-desktop/src/
├── lib.rs              — module map (kept short, this doc is the long form)
├── main.rs             — `main()` + window setup
├── shell.rs            — top-level Shell view: header + nav + body slot
├── state.rs            — AppState + AppEvent + workers + Route enum
├── api/                — typed HTTP client + serde wire types
├── sse.rs              — long-lived SSE worker → AppEvent::Sse
├── routes/             — body content per Route variant
├── theme.rs            — Palette struct + named presets + env-var picker
├── theme_helpers.rs    — small per-tone helpers shared across routes
├── icons.rs            — line-icon set (sym / refs / ... / mcp)
├── settings.rs         — palette switcher + density toggle
├── about.rs            — modal
├── toasts.rs           — visible-toasts strip
├── native.rs           — dock badge, tray, OS hooks
├── services.rs         — lightweight service registry helpers
├── terminal/           — embedded terminal (alacritty)
└── graph_layout.rs     — pure-compute force-directed layout
```

The crate is **excluded from the workspace** because `gpui-component`
pins `tree-sitter = "0.25"` and `crabcc-core` is on `tree-sitter
0.22`. They talk to each other over HTTP/SSE on loopback only — no
path dep. Build with `cd crates/crabcc-desktop && cargo run --release`.

CI does not compile this crate (memory budget). Local quality gate
before push: `cargo fmt && cargo clippy --all-targets -- -D warnings &&
cargo test --lib`.

---

## 2. Data flow (one direction)

```
                ┌──────────────────┐
                │ HTTP server      │  crabcc-viz on loopback
                │  (REST + SSE)    │
                └─────┬─────┬──────┘
                      │     │
   GET (one-shot)     │     │  GET (long-lived SSE)
                      │     │
              ┌───────▼─┐ ┌─▼─────────┐
              │prefetch │ │ sse worker│
              │worker   │ │           │  +  telemetry_worker (3s)
              └────┬────┘ └─────┬─────┘  +  memory_worker    (10s)
                   │            │
                   └────┬───────┘
                        │
                   AppEvent  (flume::Sender)
                        │
                        ▼
        AppState::pump_events()  ← cx.spawn(async ...)
                        │
                        ▼
                  AppState mutates
                        │
                        ▼
                   cx.notify()
                        │
                        ▼
                Shell + active route re-render
```

Key choices:

- **`flume` for the channel.** smol-friendly, MPSC-cheap, `try_send`
  drops cleanly when no receiver. The worker side never blocks.
- **Workers own their tick loops.** No central scheduler — each spawn
  holds a `tx: flume::Sender<AppEvent>`. When `tx.is_disconnected()`,
  the thread exits cleanly.
- **`AppEvent` is the only boundary.** Workers never touch
  `AppState` directly. Adding a new background fetch = new
  `AppEvent` variant + new worker thread + match arm in
  `AppState::apply`.
- **One render path.** Every state mutation finishes with
  `cx.notify()`. The Shell view + active route observe AppState
  via `cx.observe(...)` and redraw on each notify.

---

## 3. Routing model

There is no router library. `Route` is a small enum:

```rust
pub enum Route {
    Home, Agents, Logs, System,
    Knowledge, Commands, Timeline, KnowledgeGraph,
}
```

`AppState::route` holds the current variant. The Shell view's body
is a single `match` on it. Each route's view *entity* is constructed
once and reused — switching route doesn't drop them. State that's
local to the route (filter input, selection, layout cache) survives
route changes; AppState holds only what's shared across routes.

Adding a route: new `Route` variant + new view module under
`routes/` + one match arm in `Shell::render`'s body match + nav strip
entry in `Route::ALL`.

---

## 4. Theme / Palette

`Palette` is a value-type struct of `u32` RGB values, registered as
a gpui `Global<Palette>`. `gpui-component`'s built-in `Theme` covers
the core tokens (`background`, `foreground`, `muted_foreground`,
`secondary`, `border`, `primary`, `success`, `danger`); the cyberpunk
accents (`cyber_cyan`, `cyber_pink`, `cyber_amber`, `agent_text`,
`agent_muted`, `cyber_bg_deep`) live on `Palette` so per-route widgets
can read them via `cx.global::<Palette>().cyber_cyan_hsla()`.

**Adding a palette:** one `pub const NAME: Palette = Self { ... };`
in `theme.rs`. The render path doesn't change. Available palettes:
`web_dark`, `web_light`, `cyberpunk_neon`, `mono`, `high_contrast`,
`solarized_dark`, `dracula`. Override at runtime:

```sh
CRABCC_DESKTOP_PALETTE=cyberpunk_neon cargo run --release
```

`theme_helpers::op_color(op, theme)` maps tool families
(`sym`, `refs`, `callers`, `outline`, `fuzzy`, `prefix`,
`memory.ingest`, ...) to consistent colours across routes.

---

## 5. Cross-route navigation handoffs

`AppState::route` is just an enum — it can't carry payload. To
navigate *from* one route *into* another with context (selected id,
active filter, etc.), routes use **one-shot staging slots** on
`AppState`:

```rust
// AppState
pub pending_timeline_agent_pin: Option<SharedString>,   // → Timeline.agent_pin
pub pending_timeline_op_pin:    Option<SharedString>,   // → Timeline.op_pin
pub pending_agents_selected_id: Option<SharedString>,   // → Agents.selected_id
pub pending_knowledge_filter:   Option<SharedString>,   // → Knowledge.filter_lower
pub pending_knowledge_wing_pin: Option<SharedString>,   // → Knowledge.wing_pin
pub pending_kgraph_selected_id: Option<SharedString>,   // → K-Graph.selected
pub pending_spawn_profile:      Option<SharedString>,   // → SpawnSheet.selected_profile
```

Each slot has:

- a `navigate_to_X_with_*(value)` setter that sets the slot AND
  flips `route` in one call;
- a `take_pending_X()` consumer the target route calls in its
  `Render::render` to read-and-clear (`Option::take`).

**Pattern:**

```rust
// Sender (any route's click handler)
self.state.update(cx, |s, cx| {
    s.navigate_to_timeline_with_agent_pin(agent_id);
    cx.notify();
});

// Receiver (target route's Render)
let pending = self.state.update(cx, |s, _| s.take_pending_timeline_agent_pin());
if let Some(id) = pending {
    self.agent_pin = Some(id);
}
```

**One-shot semantics matter.** A non-take read would re-apply the
pin on every notify tick, fighting any manual deselect. `Option::take`
gives "applied exactly once" for free.

The handoff slots live alongside each other so a future entry point
can stage multiple at once — Knowledge's render consumes
`pending_knowledge_filter` and `pending_knowledge_wing_pin` in a
single `state.update` for that reason.

**Current cross-route handoffs (9 directions):**

| From → To              | Carries     | Setter                                      |
|------------------------|-------------|---------------------------------------------|
| Dashboard agent → Time | agent_id    | navigate_to_timeline_with_agent_pin         |
| Dashboard op → Time    | op string   | navigate_to_timeline_with_op_pin            |
| Agents row → Time      | agent_id    | navigate_to_timeline_with_agent_pin         |
| Time → Agents          | agent_id    | navigate_to_agents_with_selection           |
| System kills → Agents  | run_id      | navigate_to_agents_with_selection           |
| K-Graph node → Know.   | drawer id   | navigate_to_knowledge_with_filter           |
| K-Graph wing → Know.   | wing name   | navigate_to_knowledge_with_wing_pin         |
| Know. row → K-Graph    | source_id   | navigate_to_kgraph_with_selection           |
| System profile → Home  | profile_id  | navigate_to_dashboard_with_spawn_profile    |

---

## 6. Click-to-copy pattern

Many surfaces carry paste-ready strings (paths, URLs, ids). The
shared shape:

```rust
let payload = value.clone();
let tooltip_text: SharedString =
    SharedString::from(format!("Click to copy \u{201C}{value}\u{201D}"));
div()
    .id(unique_id)
    .px_1()
    .rounded_md()
    .text_color(muted)            // or `primary` for actionable
    .cursor_pointer()
    .hover(move |s| s.bg(secondary))
    .tooltip(move |w, cx| Tooltip::new(tooltip_text.clone()).build(w, cx))
    .child(display_string)        // may differ from payload (e.g. leaf vs full path)
    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
        cx.stop_propagation();    // critical when row also click-handles
        cx.write_to_clipboard(ClipboardItem::new_string(payload.to_string()));
    })
```

**`cx.stop_propagation()` is non-optional** when the chip lives
inside a row that has its own click handler (toggle log panel,
collapse section, navigate, etc.). Forgetting it means a click on
the chip fires two unrelated actions.

For URL / path / endpoint values where the displayed text equals
the clipboard payload, `routes::system::copy_chip(id, value, muted,
secondary)` collapses this to one call.

For values where display ≠ payload (root chip shows leaf, copies
full path; drawer id shows `#42`, copies `web:abc123`), inline
the click handler — the helper deliberately ties display + payload
together to keep the common case typo-proof.

---

## 7. Worker plumbing details

`AppState::workers: Option<WorkerHandles>` holds the `flume::Sender`
+ base URL after `state::build` runs. `Default::default()` is
`None` (so the type derives `Default`); `build` populates it
before any view reads. One-shot user actions (`submit_memory_graph`,
`submit_command_run`, `submit_kill`, etc.) clone the sender, spawn a
detached thread, send the result back as the appropriate
`AppEvent::*Result` variant, exit.

```rust
pub fn submit_memory_graph(&self) {
    let Some(handles) = self.workers.clone() else { return; };
    let WorkerHandles { tx, base_url } = handles;
    std::thread::Builder::new()
        .name("crabcc-memory-graph".into())
        .spawn(move || {
            let client = Client::with_base_url(base_url);
            let result = client.memory_graph();
            let _ = try_send_app_event(&tx, AppEvent::MemoryGraphResult(result));
        })
        .expect("memory-graph thread spawn");
}
```

Why separate threads instead of `cx.spawn`? Two reasons:

1. The HTTP client (reqwest blocking) wants a real OS thread — it
   doesn't play well with smol's executor.
2. A panicking handler kills only one thread. The pump task that
   drains the channel (running on `cx.spawn`) is the
   single-point-of-failure we *do* care about; the workers are
   replaceable.

---

## 8. SSE event model

`sse.rs` connects to `/api/events`, parses JSON event frames,
emits `AppEvent::Sse(SseEvent)`. `SseEvent` is an enum mirroring
the server's tagged event types (`agents`, `services`,
`activity`, `prefetch`, `agent_launch`, `agent_kill`, ...).
`AppState::apply` matches on the variant and updates the
relevant slots.

The SSE worker auto-reconnects with exponential backoff on
disconnect; it does *not* surface reconnect attempts to the UI
(too noisy on flaky networks). A persistent disconnect manifests
as stale data in the dashboard — an explicit pill in the header
flags it.

---

## 9. Conventions

- **Filter inputs.** Every filterable route uses
  `gpui-component::input::InputState`. Keep a lower-cased mirror
  (`String`) on the route, kept in sync via
  `cx.subscribe_in` on `InputEvent::Change`. Match against the
  mirror, not by re-lowercasing on every render.
- **Wrapper border on focus.** Wrap the `Input` in a `div` whose
  `border_color` flips to `primary` while focused. gpui-component's
  own input chrome is intentionally minimal; this gives the user
  the "you're typing here" cue.
- **Empty-state component.** `routes::empty::empty_state(glyph,
  title, description, muted, foreground)` for "nothing to show"
  panels. Centred, glyph-led, never silent.
- **Tooltips for state-aware actions.** Click-targets that toggle
  state (pin / unpin, expand / collapse) carry a tooltip whose
  text reflects what the *next* click will do. e.g. "Collapse
  Agents" while expanded; "Expand Agents" while collapsed.
- **Element IDs.** Stateful elements (those with a click handler)
  must declare `.id(...)`. For per-row interactives, suffix with a
  stable per-row key (drawer id, agent id, idx) so the id is
  unique within one render pass.
- **Pre-compute element ids.** Where the row's id is built from a
  `SharedString` field, pre-compute and store on a `Derived`
  helper struct (e.g. `AgentDerived`) so the format!() doesn't
  fire on every render.

---

## 10. Adding a new surface — the checklist

1. **Wire types** in `api/types.rs` (`#[derive(Deserialize)]`,
   `#[serde(default)]` for any optional field).
2. **Client method** in `api/client.rs` returning `Result<T>`.
3. **AppState slot** + apply-arm + (if user-initiated) a
   `submit_*` helper that spawns a thread.
4. **Route view** under `routes/` — observe AppState, pull the
   slot in `Render`, render with the conventions above.
5. **Cross-route nav?** If the surface should accept context from
   another route, add a `pending_X` slot + setter + take helper.
6. **Tests.** Pure helpers (matchers, formatters) get unit tests
   in the same module. State helpers — `navigate_to_*` setters,
   one-shot `take_*` consumers — get tests in `state.rs::tests`
   (we have a fixed pattern: setter sets route + slot; take
   returns Some-then-None).

---

## 11. What lives where

| Concern                          | Module                                   |
|----------------------------------|------------------------------------------|
| Window / app entry               | `main.rs`                                |
| Top-level chrome (nav, header)   | `shell.rs`                               |
| Domain state                     | `state.rs`                               |
| HTTP client                      | `api/client.rs`                          |
| Wire types                       | `api/types.rs`                           |
| SSE pump                         | `sse.rs`                                 |
| Per-route views                  | `routes/<route>.rs`                      |
| Theme + palettes                 | `theme.rs`                               |
| Per-tone helpers                 | `theme_helpers.rs`                       |
| Toasts                           | `toasts.rs`                              |
| Embedded terminal                | `terminal/`                              |
| About modal                      | `about.rs`                               |
| Settings panel                   | `settings.rs`                            |
| Force-directed graph layout      | `graph_layout.rs`                        |
| OS hooks (dock badge, etc.)      | `native.rs`                              |
| Tool-family icon set             | `icons.rs`                               |

---

## 12. References

- `crates/crabcc-desktop/docs/DESIGN-BRIEF.md` — per-route product
  brief (the *what*).
- `AGENTS.md` — repo-wide agent guidance.
- `CLAUDE.md` — Claude Code-specific tips for working in this repo.
- `crates/crabcc-core/docs/HOW_IT_WORKS.md` — server-side data model +
  indexing pipeline.
