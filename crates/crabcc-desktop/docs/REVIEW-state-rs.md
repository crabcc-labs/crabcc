# REVIEW — `crates/crabcc-desktop/src/state.rs`

> Senior-Rust review focused on advanced data-type / byte-trick wins.
> Read-only review — no source touched. Style matches the in-tree
> comments in `state.rs`: terse, dashes, no fluff.

File reviewed: ~470 lines, single-window dashboard model.
Workers: prefetch (one-shot) + SSE bridge + telemetry poll (3 s) +
memory poll (10 s) → single `flume::unbounded` → `pump_events` drain.

---

## Top 5 highest-impact items

### 1. `AppEvent` non-`Initial` variants still carry sizeable inline payloads

- **Location**: `enum AppEvent`, lines 78–108.
- **Current**: only `Initial` is boxed (1.6 KB note in the doc). The
  remaining variants store `anyhow::Result<TelemetrySnapshot>` /
  `Result<MemoryRecentResponse>` etc. inline. `anyhow::Error` is
  pointer-sized but `TelemetrySnapshot` and `MemoryRecentResponse`
  carry inline `Vec<…>` / `String` fields — the enum's stack size is
  set by the largest non-boxed variant.
- **Suggestion**: run `cargo +nightly rustc -- -Zprint-type-sizes
  2>&1 | grep AppEvent` and box every variant whose payload exceeds
  ~64 B. Cheapest fix:
  ```rust
  Telemetry(anyhow::Result<Box<TelemetrySnapshot>>),
  MemoryRefresh(anyhow::Result<Box<MemoryRecentResponse>>),
  ```
  Alternatively a custom `enum TelemetryOutcome {
  Ok(Box<TelemetrySnapshot>), Err(anyhow::Error) }` so the niche of
  `Box` keeps the discriminant free.
- **Win**: every `flume` slot drops from `sizeof(largest variant)` to
  ~24 B. With an unbounded queue absorbing a 256-event burst the
  steady-state RSS drop is real (medium — memory). Channel send/recv
  also memcpys less per message (small — ns).
- **Risk**: minimal; one extra heap alloc per poll tick (already
  allocating internally). Boxing changes pattern syntax in `apply`
  (`let snapshot = *snapshot;` shim only).

### 2. `last_error` / `last_ingest` / `last_launch` / `last_kill` allocate `String` from `format!` on every event

- **Location**: lines 182–191; writers at lines 221–253, 284–333.
- **Current**: every error path runs `format!("bootstrap: {e}")`
  which allocates. Status strings on the happy paths same. They live
  in `AppState` long enough for the next event to overwrite them.
- **Suggestion**:
  - For prefetch-error tags (`"bootstrap"`, `"services"`, …) the
    prefix is one of nine `&'static str`s. Either store them as
    `(prefix: &'static str, body: String)` and `Display` lazily, or
    use `compact_str::CompactString` so the typical "bootstrap: " +
    30-char message stays inline (24 B SSO) and skips the heap.
  - Replace `Option<String>` with `Option<CompactString>` (or
    `Option<Box<str>>` if you want zero deps — saves the `usize`
    capacity word per slot).
- **Win**: one alloc dropped per poll-error tick. With unbounded
  channel + 3 s telemetry poll + a flapping endpoint, ~1 alloc/3 s
  steady state; more importantly `Box<str>` shrinks every empty
  `last_error: None` slot from 24 B → 16 B (small — memory).
- **Risk**: `compact_str` is a new dep — justified by removing
  per-error-tick allocs and providing 24-byte SSO. If the bar is
  zero-deps, `Box<str>` alone gets the layout win (no SSO).

### 3. `Option<flume::Sender<AppEvent>>` + `Option<String>` — wire them into a non-default-able sub-struct

- **Location**: `AppState` lines 196–205, used in
  `submit_ingest`/`submit_launch`/`submit_kill` (lines 342–393).
- **Current**: every submit method opens with two `let Some(...) =
  self.x.clone() else { return };`. `Default` fabricates a
  half-wired `AppState` that silently swallows submits. The comment
  at line 199 literally calls out the smell.
- **Suggestion**: introduce a `Wired` substructure and store
  `Option<Wired>` once:
  ```rust
  struct Wired {
      tx: flume::Sender<AppEvent>,
      base_url: Arc<str>,
  }
  pub struct AppState { wired: Option<Wired>, ... }
  ```
  Each `submit_*` opens with `let Some(w) = self.wired.as_ref() else
  { return }` and clones one `Arc<str>` instead of cloning a
  `String` every submit. `Arc<str>` is 16 B vs. `String`'s 24 B and
  the clone is a refcount bump, not a heap alloc.
- **Win**: each `submit_*` saves one heap alloc + memcpy of the base
  URL on every button click. One `Option` instead of two. (Medium —
  ergonomics + small alloc save.)
- **Risk**: none. Internal-only refactor.

### 4. The three `submit_*` methods are byte-for-byte identical bar one closure body

- **Location**: lines 342–393.
- **Current**: triple-copy of `Option<Sender>::clone` +
  `Option<String>::clone` +
  `thread::Builder::name(...).spawn(move || { Client::with_base_url(base);
  ... tx.send(AppEvent::Variant(...)) })`. ~50 LoC duplication.
- **Suggestion**: a small private helper that takes a `&'static str`
  (thread name) and a `FnOnce(&Client, &Sender) -> ()`:
  ```rust
  fn dispatch<F>(&self, name: &'static str, f: F)
  where
      F: FnOnce(&Client, &flume::Sender<AppEvent>) + Send + 'static,
  {
      let Some(w) = self.wired.as_ref() else { return };
      let tx = w.tx.clone();
      let base = Arc::clone(&w.base_url);
      std::thread::Builder::new()
          .name(name.into())
          .spawn(move || {
              let client = Client::with_base_url(base.to_string());
              f(&client, &tx);
          })
          .expect("dispatch thread spawn");
  }
  ```
  `submit_ingest` becomes 4 lines and the follow-up `MemoryRefresh`
  send stays clear. Avoid a macro — function works.
- **Win**: ~40 LoC dropped, one place to thread `Wired` through
  (large — maintainability; nil — runtime).
- **Risk**: trait-object boxing of the closure costs one heap alloc
  per submit, but submits are click-driven (≤1 Hz). Acceptable.

### 5. `ACTIVITY_BUFFER` / `TELEMETRY_BUFFER` writes use `len() == CAP { pop_front }; push_back` against an un-pre-allocated `VecDeque`

- **Location**: lines 256–267 (activity), 274–282 (telemetry).
- **Current**: `VecDeque<T>` with manual cap. Each push checks `==
  ACTIVITY_BUFFER`. The deque was allocated with default capacity
  (8) and grows by doublings to 64 / 256, never shrinks.
- **Suggestion**: pre-allocate at construction:
  ```rust
  recent_activity: VecDeque::with_capacity(ACTIVITY_BUFFER),
  telemetry: VecDeque::with_capacity(TELEMETRY_BUFFER),
  ```
  Removes the 4 reallocations during warm-up. Better, swap to
  `heapless::HistoryBuffer<T, N>` or a hand-rolled SPSC ring (the
  consumer is single-threaded — `pump_events`). For a 1-producer
  1-consumer setup `crossbeam_queue::ArrayQueue` would let the
  poller push without going through the central channel at all, but
  that's a bigger refactor.
- **Win**: 4 reallocs eliminated on startup; cache locality improves
  for the hot Logs render path. Fixed-N ring also makes
  `clippy::vec_box` happy. (Small — ns; medium — startup time.)
- **Risk**: `with_capacity` is zero-risk. `heapless` adds a dep —
  justified only if you also adopt it elsewhere; otherwise stick
  with `with_capacity`.

---

## Minor appendix

### M1. `Route::ALL` / `label`

- **Location**: lines 122–141.
- Nothing crucial. `Route` is `Copy`-cheap and the `match` in
  `label` LLVM-optimises to a jump table. If routes ever exceed 8,
  swap to a `&'static [(&'static str, Route)]` table to avoid
  duplicating the order between `ALL` and `label`.
- **Win/Risk**: nil / nil.

### M2. `agents_running` recomputes by linear scan on every render

- **Location**: lines 399–405.
- **Current**: `O(n)` per call where `n = agents.len()` (typically
  ≤16). View calls this on every redraw.
- **Suggestion**: cache a `running: u32` recomputed only inside the
  `AppEvent::Sse(SseEvent::Agents(_))` arm.
- **Win**: negligible at n=16, but eliminates per-frame iter at
  60 Hz. (Small — ns.)
- **Risk**: drift if a future code path mutates `self.agents`
  without going through `apply`. Encapsulate behind a setter.

### M3. `services_reachable` does the same scan twice

- **Location**: lines 407–412.
- **Current**: two passes (`.len()` then `.filter().count()`).
- **Suggestion**:
  ```rust
  let (up, total) = report.services.iter().fold((0u32, 0u32), |(up, total), s| {
      (up + s.reachable as u32, total + 1)
  });
  ```
- **Win**: one pass instead of two. (Tiny.)
- **Risk**: nil.

### M4. `flume::unbounded` is the safe default but a slow gpui pump can pile up

- **Location**: line 441.
- **Current**: every worker can push without backpressure. If
  `pump_events` falls behind (heavy redraw stall), the queue grows;
  telemetry events arrive with stale ordering wrt SSE.
- **Suggestion**: use `flume::bounded(256)` and have telemetry /
  memory pollers `try_send` with `Full` → drop the tick; the next
  poll covers ground. Prefetch + SSE keep blocking-send. This is
  exactly the producer/consumer cardinality story the brief flagged.
- **Win**: bounded memory under pathological gpui stalls,
  predictable worst-case channel size. (Medium — robustness.)
- **Risk**: dropped telemetry ticks may surface as gaps in Logs;
  but the cursor advances inside `apply` only, so a dropped tick
  replays cleanly on the next.

### M5. `format!(" pid {p}")` / `format!(" · {n}")` — preassemble lazily

- **Location**: lines 304–329 (`AgentLaunchResult`,
  `AgentKillResult`).
- **Current**: helper closures `.map(|p| format!(...))` then
  `.unwrap_or_default()`. Two heap allocs on every kill.
- **Suggestion**: build into a single `String::with_capacity(64)`
  via `write!`. Empty branches stay alloc-free.
- **Win**: 1 alloc per submit. (Tiny — submits are click-rate.)
- **Risk**: nil.

### M6. `i64` cursor inside the telemetry worker but `u64` on the wire

- **Location**: line 500 vs. `self.telemetry_cursor: u64` (line 174)
  and `snapshot.cursor` (`u64`).
- **Current**: `cursor: i64 = 0;` and `snapshot.cursor as i64` in
  the worker. Two casts; could go wrong if a cursor overflows
  `i64::MAX`.
- **Suggestion**: keep both sides `u64`. If `client.telemetry`
  takes `Option<i64>` for HTTP query-string reasons — fix the
  signature, not the cast.
- **Win**: one less casting bug class. (Correctness.)
- **Risk**: cross-crate signature change.

### M7. `Prefetch::apply` arm is a 9-way `match Ok/Err` ladder

- **Location**: lines 217–254.
- **Current**: 9 copy-pasted match arms. New field forgets to
  update here → silently drops the result.
- **Suggestion**: a private helper `fn apply_field<T>(field: &mut
  Option<T>, r: anyhow::Result<T>, tag: &'static str, err: &mut
  Option<String>)` + 9 calls. Halves the LoC.
- **Win**: maintainability. (Medium.)
- **Risk**: nil.

### M8. `Box<dyn Trait>` audit

- None observed. The four worker spawns are concrete closures.
  The `dispatch` helper in item 4 introduces one `Box<dyn FnOnce>`
  per submit, bounded by user click rate. Acceptable.

### M9. `last_kill` etc. are `Option<Result<String, String>>` — flatten

- **Location**: lines 187–191.
- **Current**: `Option<Result<String, String>>` is 48 B (Option
  discriminant + Result discriminant + two `String`s the active
  arm picks between, plus padding).
- **Suggestion**: a tagged enum:
  ```rust
  enum SubmitStatus { Idle, Ok(CompactString), Err(CompactString) }
  ```
  With `CompactString` (or `Box<str>`) the active variant is 16 B
  + 1 B tag → 24 B. Three of these on `AppState` → ~72 B saved on
  the struct.
- **Win**: 72 B per `AppState` and a clearer state machine.
- **Risk**: callers' pattern syntax changes.

### M10. `_app_marker(_app: &App)` dead-code stub

- **Location**: lines 562–564.
- **Suggestion**: delete it or `#[cfg(any())]`-gate it. The
  `#[allow(dead_code)]` lint suppression is a smell — comment says
  "until A.5 needs it"; if A.5 is in flight, wire it.
- **Win**: clarity.
- **Risk**: nil.

### M11. `Mutex<Regex>` audit

- None. No regexes touched in this file. Logged for completeness
  per brief.

### M12. Drop-on-separate-thread (Wild-linker trick, #216)

- The biggest payload is the `Box<Prefetch>` → consumed by `apply`
  once. After consumption it drops on the gpui thread
  (`pump_events` async block). For a 1.6 KB struct with 9 nested
  `Result`/`Vec` heap blocks the drop is non-trivial (each `Vec`
  = free-list call). Could spawn a "graveyard" thread:
  ```rust
  static GRAVEYARD: OnceLock<flume::Sender<Box<dyn Send>>> = ...
  ```
  and ship the boxed `Prefetch` there after extracting fields in
  `apply`. Same trick applies to `TelemetrySnapshot` once boxed
  per item 1.
- **Win**: removes ~9 free-list calls from the gpui frame budget at
  prefetch-completion. (Tiny once, but if the pattern repeats for
  large memory snapshots @ 10 s the cost amortises.)
- **Risk**: graveyard thread leaks if app crashes mid-shutdown —
  but bounded to app lifetime.

---

## Summary — top 5

| # | Where | Win | Risk |
|---|-------|-----|------|
| 1 | `AppEvent` non-`Initial` variants | per-message memcpy ↓; channel slot 1.6 KB → 24 B | one extra alloc per tick |
| 2 | `last_*: String` → `CompactString` / `Box<str>` | 1 alloc dropped per error tick; SSO inline | new dep for SSO |
| 3 | `Option<Sender>+Option<String>` → `Option<Wired>` w/ `Arc<str>` | 1 alloc dropped per submit; clarity | nil |
| 4 | `submit_*` triplicate → `dispatch` helper | -40 LoC | one boxed closure per submit |
| 5 | `VecDeque::with_capacity` (or fixed ring) | 4 reallocs at startup; cache | nil for `with_capacity` |

---

## Cross-references

- Initiative: #213 (native desktop & rich notifications kickoff).
- Wild-linker-style perf followups: #216 (drop-on-separate-thread,
  reuse_vec). M12 here is exactly that pattern applied to
  `Prefetch`.
