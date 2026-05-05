//! MCP Inspector — tool-call ring buffer + view types.
//!
//! M0 implementation. In-memory ring only; payloads inlined as
//! pretty-printed JSON strings. Future milestones add SQLite
//! durability and a content-addressed payload store (see
//! `crates/crabcc-desktop/docs/MCP-INSPECTOR.md` §3).
//!
//! The source connection is deferred to M1: the in-proc MCP
//! bridge that emits `CallEvent`s into the ring doesn't exist yet.
//! Once it does, every MCP message handler calls
//! [`crate::state::AppState::record_inspector_event`].

use gpui::SharedString;
use std::sync::atomic::{AtomicU64, Ordering};

/// Ring-buffer capacity. Spec §3.1 calls for 10 k; M0 starts at
/// 1 024 to keep memory pressure trivial during initial
/// development. Bump once the source pump is wired and we have a
/// realistic event rate to size against.
pub const INSPECTOR_RING_CAP: usize = 1024;

/// Hard cap on inline payload size (bytes). Larger payloads are
/// truncated with a `… [N more bytes]` sentinel; M0+2 swaps this
/// for a content-addressed payload store and removes the cap.
pub const MAX_PAYLOAD_INLINE: usize = 16 * 1024;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Peer → us.
    In,
    /// Us → peer.
    Out,
}

impl Direction {
    pub fn glyph(self) -> &'static str {
        match self {
            Direction::In => "\u{2190}",  // ←
            Direction::Out => "\u{2192}", // →
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pending,
    Ok,
    Err { code: i32, msg: SharedString },
}

impl Status {
    pub fn glyph(&self) -> &'static str {
        match self {
            Status::Pending => "\u{23F3}", // ⏳
            Status::Ok => "\u{2713}",      // ✓
            Status::Err { .. } => "\u{26A0}", // ⚠
        }
    }
    pub fn is_err(&self) -> bool {
        matches!(self, Status::Err { .. })
    }
    pub fn is_pending(&self) -> bool {
        matches!(self, Status::Pending)
    }
}

/// Filter selector for the timeline view. `Status::Err` doesn't
/// implement `Copy`, so this enum is used for filter equality
/// rather than `Status` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Any,
    Pending,
    Ok,
    Err,
}

impl StatusKind {
    pub fn label(self) -> &'static str {
        match self {
            StatusKind::Any => "any",
            StatusKind::Pending => "pending",
            StatusKind::Ok => "ok",
            StatusKind::Err => "err",
        }
    }
    pub fn matches(self, status: &Status) -> bool {
        match (self, status) {
            (StatusKind::Any, _) => true,
            (StatusKind::Pending, Status::Pending) => true,
            (StatusKind::Ok, Status::Ok) => true,
            (StatusKind::Err, Status::Err { .. }) => true,
            _ => false,
        }
    }
}

/// One MCP message recorded for the inspector.
///
/// M0-shaped: payloads stored inline as pretty-printed JSON
/// strings (capped at [`MAX_PAYLOAD_INLINE`]). The full schema in
/// `MCP-INSPECTOR.md` §2 (Ulid id, content-addressed payload refs,
/// transport, agent_origin, replay_of, sampling telemetry,
/// consent ref, …) is grown into across M0+1, M0+2, M3, M4.
#[derive(Debug, Clone)]
pub struct CallEvent {
    pub id: u64,
    /// Unix millis. i64 matches the project's existing timestamp
    /// convention (see `state.rs::AppState::last_event_ts`).
    pub ts_ms: i64,
    pub server: SharedString,
    pub direction: Direction,
    pub method: SharedString,
    /// For `tools/call`, the unwrapped tool name. `None` for any
    /// other method.
    pub tool_name: Option<SharedString>,
    pub status: Status,
    pub latency_ms: Option<u32>,
    pub params_pretty: SharedString,
    pub result_pretty: Option<SharedString>,
    /// Causality link to a parent event id (e.g. a sampling call
    /// triggered as a child of a tool call). `None` at the root.
    pub parent_id: Option<u64>,
}

impl CallEvent {
    /// Process-wide monotonic id. Resets on restart, which is fine
    /// for M0 — durable id allocation comes with the SQLite layer
    /// (M0+1).
    pub fn next_id() -> u64 {
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    }
}

/// Seed the inspector ring with a handful of synthetic events for
/// rendering validation. Wired into [`crate::state::build`] behind
/// the `CRABCC_DESKTOP_INSPECTOR_DEMO` env var so production
/// binaries don't carry demo state. Every event uses freshly
/// minted ids from [`CallEvent::next_id`], so calling this twice
/// is harmless (no id collisions).
///
/// Eight events: a sampling parent/child pair (depth chain), an
/// erroring tool call, a consent grant, a resource read, plus the
/// usual mix of inbound / outbound traffic across three peers
/// (`self`, `agent-42`, `slack`).
pub fn seed_demo_events(state: &mut crate::state::AppState) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // Allocate ids upfront so parent_id wiring is straightforward.
    let id_list = CallEvent::next_id();
    let id_grant = CallEvent::next_id();
    let id_slack = CallEvent::next_id();
    let id_fff_err = CallEvent::next_id();
    let id_mem = CallEvent::next_id();
    let id_resread = CallEvent::next_id();
    let id_sample = CallEvent::next_id();
    let id_sample_done = CallEvent::next_id();

    let mk = |id: u64,
              offset_ms: i64,
              server: &'static str,
              direction: Direction,
              method: &'static str,
              tool: Option<&'static str>,
              status: Status,
              latency_ms: Option<u32>,
              params: &str,
              result: Option<&str>,
              parent: Option<u64>|
     -> CallEvent {
        CallEvent {
            id,
            ts_ms: now_ms - offset_ms,
            server: SharedString::new_static(server),
            direction,
            method: SharedString::new_static(method),
            tool_name: tool.map(SharedString::new_static),
            status,
            latency_ms,
            params_pretty: SharedString::from(params.to_string()),
            result_pretty: result.map(|r| SharedString::from(r.to_string())),
            parent_id: parent,
        }
    };

    let events = vec![
        mk(
            id_list,
            12_000,
            "self",
            Direction::In,
            "tools/list",
            None,
            Status::Ok,
            Some(2),
            "{}",
            Some(r##"{"tools": [/* 14 tools */]}"##),
            None,
        ),
        mk(
            id_grant,
            10_500,
            "self",
            Direction::Out,
            "consent/grant",
            None,
            Status::Ok,
            Some(0),
            r##"{"peer": "slack", "tool": "slack.send_message", "scope": "session"}"##,
            Some(r##"{"granted": true}"##),
            None,
        ),
        mk(
            id_slack,
            9_800,
            "slack",
            Direction::In,
            "tools/call",
            Some("slack.send_message"),
            Status::Ok,
            Some(184),
            r##"{
  "channel": "#engineering",
  "text": "build is green"
}"##,
            Some(r##"{"ts": "1746461213.001", "ok": true}"##),
            None,
        ),
        mk(
            id_fff_err,
            8_400,
            "fff",
            Direction::In,
            "tools/call",
            Some("fff.grep"),
            Status::Err {
                code: -32603,
                msg: SharedString::new_static("upstream timeout after 5s"),
            },
            Some(5_002),
            r##"{
  "patterns": ["TODO"],
  "constraints": "**/*.rs"
}"##,
            None,
            None,
        ),
        mk(
            id_mem,
            6_900,
            "self",
            Direction::In,
            "tools/call",
            Some("desktop.memory.search"),
            Status::Ok,
            Some(11),
            r##"{"query": "ollama mlx", "limit": 5}"##,
            Some(r##"{"hits": 3, "drawers": ["#147", "#288", "#412"]}"##),
            None,
        ),
        mk(
            id_resread,
            5_100,
            "self",
            Direction::In,
            "resources/read",
            None,
            Status::Ok,
            Some(3),
            r##"{"uri": "desktop://logs/agent-42"}"##,
            Some(r##"{"contents": [/* 256 KB tail */]}"##),
            None,
        ),
        // Sampling — parent (the agent's request)…
        mk(
            id_sample,
            3_200,
            "agent-42",
            Direction::In,
            "sampling/createMessage",
            None,
            Status::Ok,
            Some(847),
            r##"{
  "messages": [{"role": "user", "content": {"type": "text", "text": "summarise diff"}}],
  "modelPreferences": {"hints": [{"name": "qwen3.5"}], "costPriority": 0.9},
  "maxTokens": 2048,
  "_meta": {"samplingDepth": 0}
}"##,
            Some(
                r##"{
  "role": "assistant",
  "content": {"type": "text", "text": "Refactor extract_file_with_edges to..."},
  "model": "ollama/qwen3.5:35b-a3b-coding-nvfp4",
  "stopReason": "endTurn"
}"##,
            ),
            None,
        ),
        // …and a follow-up nested sampling at depth 1 (e.g. the
        // chosen model's tool-use loop calling back into us).
        mk(
            id_sample_done,
            3_050,
            "agent-42",
            Direction::In,
            "sampling/createMessage",
            None,
            Status::Pending,
            None,
            r##"{
  "messages": [{"role": "user", "content": {"type": "text", "text": "render that as a patch"}}],
  "modelPreferences": {"hints": [{"name": "qwen3.5"}]},
  "maxTokens": 1024,
  "_meta": {"samplingDepth": 1}
}"##,
            None,
            Some(id_sample),
        ),
    ];

    for e in events {
        state.record_inspector_event(e);
    }
}

/// `SamplingObserver` impl that ships sampling lifecycle events
/// into the inspector ring via the existing `AppEvent` flume
/// channel. Constructed once at app startup and shared across the
/// sampling handler.
///
/// Two `CallEvent`s per sampling round-trip (per spec
/// `MCP-INSPECTOR.md` §9): a `Pending` request row plus a
/// completed `Ok`/`Err` row whose `parent_id` points at the
/// request. The completed row carries `latency_ms`; the request
/// row carries the full params blob.
pub struct InspectorSamplingObserver {
    tx: flume::Sender<crate::state::AppEvent>,
}

impl InspectorSamplingObserver {
    pub fn new(tx: flume::Sender<crate::state::AppEvent>) -> Self {
        Self { tx }
    }
}

impl crate::sampling::SamplingObserver for InspectorSamplingObserver {
    fn on_request(
        &self,
        request: &crate::sampling::SamplingRequest,
        chosen_model: &str,
    ) -> u64 {
        let id = CallEvent::next_id();
        let params = serde_json::to_string_pretty(request).unwrap_or_else(|_| "{}".into());
        let evt = CallEvent {
            id,
            ts_ms: now_ms(),
            server: SharedString::new_static("self"),
            direction: Direction::In,
            method: SharedString::new_static("sampling/createMessage"),
            // Surface the chosen model in the inspector's tool-name
            // column so the row reads
            // "sampling/createMessage · ollama/qwen3.5:35b…".
            tool_name: Some(SharedString::from(chosen_model.to_string())),
            status: Status::Pending,
            latency_ms: None,
            params_pretty: truncate_inline(params),
            result_pretty: None,
            parent_id: None,
        };
        // Best-effort: drop on closed channel (gpui pump gone,
        // app shutting down). Instrumentation must never break the
        // call path.
        let _ = self
            .tx
            .send(crate::state::AppEvent::InspectorRecord(evt));
        id
    }

    fn on_response(
        &self,
        request_id: u64,
        result: &Result<crate::sampling::SamplingResponse, crate::sampling::SamplingError>,
        latency_ms: u32,
    ) {
        let id = CallEvent::next_id();
        let (status, result_pretty) = match result {
            Ok(r) => {
                let body = serde_json::to_string_pretty(r).unwrap_or_else(|_| "{}".into());
                (Status::Ok, Some(truncate_inline(body)))
            }
            Err(e) => (
                Status::Err {
                    code: e.kind.code(),
                    msg: SharedString::from(e.message.clone()),
                },
                None,
            ),
        };
        let evt = CallEvent {
            id,
            ts_ms: now_ms(),
            server: SharedString::new_static("self"),
            direction: Direction::Out,
            method: SharedString::new_static("sampling/createMessage"),
            tool_name: None,
            status,
            latency_ms: Some(latency_ms),
            params_pretty: SharedString::new_static("(see parent request row)"),
            result_pretty,
            parent_id: Some(request_id),
        };
        let _ = self
            .tx
            .send(crate::state::AppEvent::InspectorRecord(evt));
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Truncate `s` to [`MAX_PAYLOAD_INLINE`] bytes on a UTF-8 char
/// boundary, appending an `… [N more bytes]` sentinel when the
/// cap fires. Returns a `SharedString` so callers don't pay an
/// extra allocation for the common no-truncation path.
pub fn truncate_inline(s: String) -> SharedString {
    if s.len() <= MAX_PAYLOAD_INLINE {
        return SharedString::from(s);
    }
    // Reserve room for the sentinel so the final string still fits
    // under the cap. 32 bytes is generous for `… [N more bytes]`
    // even at u64::MAX.
    let target = MAX_PAYLOAD_INLINE.saturating_sub(32);
    let mut idx = target;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    let dropped = s.len() - idx;
    let mut out = s;
    out.truncate(idx);
    out.push_str(&format!("\u{2026} [{dropped} more bytes]"));
    SharedString::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_is_monotonic() {
        let a = CallEvent::next_id();
        let b = CallEvent::next_id();
        let c = CallEvent::next_id();
        assert!(a < b && b < c, "ids must increase: {a} < {b} < {c}");
    }

    #[test]
    fn direction_glyphs_are_distinct() {
        assert_ne!(Direction::In.glyph(), Direction::Out.glyph());
    }

    #[test]
    fn status_kind_matches_correctly() {
        assert!(StatusKind::Any.matches(&Status::Ok));
        assert!(StatusKind::Any.matches(&Status::Pending));
        assert!(StatusKind::Ok.matches(&Status::Ok));
        assert!(!StatusKind::Ok.matches(&Status::Pending));
        assert!(StatusKind::Err.matches(&Status::Err {
            code: -32001,
            msg: SharedString::from("denied"),
        }));
        assert!(!StatusKind::Err.matches(&Status::Ok));
    }

    #[test]
    fn truncate_inline_passes_short_strings_unchanged() {
        let s = "short payload".to_string();
        let out = truncate_inline(s.clone());
        assert_eq!(out.as_ref(), s);
    }

    #[test]
    fn truncate_inline_caps_long_strings_with_sentinel() {
        let s = "x".repeat(MAX_PAYLOAD_INLINE * 2);
        let out = truncate_inline(s);
        assert!(out.len() <= MAX_PAYLOAD_INLINE);
        assert!(
            out.contains("more bytes"),
            "expected truncation sentinel; got {}…",
            &out[..40.min(out.len())],
        );
    }

    #[test]
    fn truncate_inline_respects_utf8_boundaries() {
        // Build a string of multi-byte chars that crosses the cap;
        // the truncation point must land on a char boundary, not
        // in the middle of an encoding.
        let unit = "\u{1F600}"; // 4-byte UTF-8
        let s = unit.repeat(MAX_PAYLOAD_INLINE);
        let out = truncate_inline(s);
        // If we'd truncated mid-codepoint, `.chars().count()`
        // would have panicked or yielded a replacement char in
        // a debug build; just touching the iterator is enough.
        let _ = out.chars().count();
    }
}
