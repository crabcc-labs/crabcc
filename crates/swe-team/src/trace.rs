//! Structured trace events — the Phase-2 seam.
//!
//! Every graph node emits one `TraceEvent` to stdout as a single JSONL line.
//! Phase 2's AgentField / eye / viz adapters consume this stream verbatim, so
//! the shape is the contract: keep it small, stable, and self-describing. We do
//! NOT translate into AgentField's format here — that adapter lives in Phase 2
//! and reads these events as its input. This module is deliberately the only
//! place that knows the wire shape.

use std::time::Instant;

use serde::Serialize;

/// The decision a gate/node reached, normalized across node types. Phase-2
/// adapters branch on `decision` rather than parsing free text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Node ran to completion with no gate semantics (plan drafted, diffs
    /// produced, synthesis done, emit).
    Produced,
    /// A gate approved (lead-dev APPROVE, reviewer APPROVE).
    Approve,
    /// A gate asked for another round (lead-dev REVISE, reviewer
    /// REQUEST-CHANGES). `note` carries the gate's notes.
    Revise,
    /// A hard stop on a rule violation (lead-dev STOP).
    Stop,
    /// A round cap was hit; the graph proceeds with a warning rather than hang.
    CapReached,
    /// The node's underlying LLM/tool call failed.
    Error,
}

/// One node transition in the graph. Serialized as a JSONL line to stdout.
#[derive(Debug, Clone, Serialize)]
pub struct TraceEvent {
    /// Stable node id, e.g. `"plan"`, `"lead_gate"`, `"fanout.safety"`,
    /// `"synth"`, `"review"`, `"self_review"`, `"emit"`.
    pub node: String,
    /// Human role label, e.g. `"planner"`, `"lead-dev"`, `"coder-safety"`.
    pub role: String,
    /// The model id this node ran against (post env-resolution).
    pub model: String,
    /// Normalized decision/outcome for this node.
    pub decision: Decision,
    /// Which gate round this event belongs to (1-based); `None` for nodes that
    /// do not loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<usize>,
    /// Wall-clock duration of the node in milliseconds.
    pub elapsed_ms: u128,
    /// Optional gate notes / error string / short summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Emit one trace event as a JSONL line to stdout. Serialization of a fixed,
/// owned struct cannot fail in practice; if it ever did we must not abort the
/// run, hence the silent fallback line.
pub fn emit(event: &TraceEvent) {
    match serde_json::to_string(event) {
        Ok(line) => println!("{line}"),
        Err(_) => println!(
            "{{\"node\":\"{}\",\"decision\":\"error\",\"note\":\"trace serialize failed\"}}",
            event.node
        ),
    }
}

/// Small timer so call sites stay terse: `Span::start(...)` then `.finish(...)`
/// produces the event with elapsed time filled in.
pub struct Span {
    node: String,
    role: String,
    model: String,
    round: Option<usize>,
    start: Instant,
}

impl Span {
    pub fn start(
        node: impl Into<String>,
        role: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            node: node.into(),
            role: role.into(),
            model: model.into(),
            round: None,
            start: Instant::now(),
        }
    }

    pub fn round(mut self, round: usize) -> Self {
        self.round = Some(round);
        self
    }

    /// Consume the span, emitting a `TraceEvent` with the measured duration.
    pub fn finish(self, decision: Decision, note: Option<String>) {
        emit(&TraceEvent {
            node: self.node,
            role: self.role,
            model: self.model,
            decision,
            round: self.round,
            elapsed_ms: self.start.elapsed().as_millis(),
            note,
        });
    }
}
