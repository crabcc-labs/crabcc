//! Default `ResourceProvider` impl backed by AppState.
//!
//! Closes the `includeContext` loop so an external MCP peer that
//! asks for `thisServer` or `allServers` actually gets a tight
//! summary of what's happening in the desktop right now: running
//! agents, recent activity events, recent memory drawers.
//!
//! The snapshot lives behind an `Arc<RwLock<...>>` shared between
//! the AppState writer (gpui thread, refreshes on every `apply()`)
//! and the provider reader (worker thread, called by the sampling
//! handler when `includeContext.injects()`). RwLock is fine here:
//! reads are infrequent (once per sampling call with includeContext
//! set), writes are frequent but cheap (string assembly only,
//! microseconds).
//!
//! Spec target: `MCP-SAMPLING-OFFER.md` §3.2 / §7 — the
//! "host's superpower over a vanilla Ollama call" piece.

use std::sync::{Arc, RwLock};

use crate::sampling::{IncludeContext, ResourceProvider, ResourceSnippet};
use crate::state::AppState;

/// Cached text snippets a [`ResourceProvider`] hands to the
/// summary lane. Default = all empty (provider returns no
/// snippets, summary lane is skipped).
#[derive(Default, Debug, Clone)]
pub struct ResourceSnapshot {
    pub agents_text: String,
    pub activity_text: String,
    pub memory_text: String,
}

/// How many of each list we fold into the snapshot. The summary
/// lane is bandwidth-limited — feeding it 1000 events is wasteful.
/// Pick the most recent N.
const MAX_AGENTS: usize = 12;
const MAX_ACTIVITY: usize = 25;
const MAX_DRAWERS: usize = 20;

impl ResourceSnapshot {
    /// Rebuild from the live AppState. Cheap — only string formatting,
    /// no I/O.
    pub fn from_state(state: &AppState) -> Self {
        Self {
            agents_text: format_agents(state),
            activity_text: format_activity(state),
            memory_text: format_memory(state),
        }
    }

    /// True when at least one section has content. Lets the
    /// provider skip emitting an empty snippet list (which would
    /// trigger a no-op summary call).
    pub fn is_empty(&self) -> bool {
        self.agents_text.is_empty()
            && self.activity_text.is_empty()
            && self.memory_text.is_empty()
    }
}

fn format_agents(state: &AppState) -> String {
    if state.agents.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for a in state.agents.iter().take(MAX_AGENTS) {
        let model = a.model.as_deref().unwrap_or("?");
        let runtime = a.runtime.as_deref().unwrap_or("?");
        let status = match a.status {
            crate::api::types::AgentStatus::Running => "running",
            crate::api::types::AgentStatus::Exited => "exited",
        };
        out.push_str(&format!(
            "- {id} · {status} · runtime={runtime} · model={model}\n",
            id = a.id,
        ));
    }
    if state.agents.len() > MAX_AGENTS {
        out.push_str(&format!(
            "  …and {} more (truncated for summary)\n",
            state.agents.len() - MAX_AGENTS,
        ));
    }
    out
}

fn format_activity(state: &AppState) -> String {
    if state.recent_activity.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    // VecDeque newest-at-back per AppState convention; iterate
    // back-to-front so the most-recent N show up.
    for evt in state.recent_activity.iter().rev().take(MAX_ACTIVITY) {
        let agent = evt
            .agent_id
            .as_deref()
            .map(|s| format!(" · agent={s}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- {ts} · {op} · q={query} · results={n}{agent}\n",
            ts = evt.ts,
            op = evt.op,
            query = evt.query,
            n = evt.results,
        ));
    }
    if state.recent_activity.len() > MAX_ACTIVITY {
        out.push_str(&format!(
            "  …and {} earlier events (truncated)\n",
            state.recent_activity.len() - MAX_ACTIVITY,
        ));
    }
    out
}

fn format_memory(state: &AppState) -> String {
    let Some(resp) = state.memory_recent.as_ref() else {
        return String::new();
    };
    if !resp.present || resp.drawers.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for d in resp.drawers.iter().take(MAX_DRAWERS) {
        let room = d.room.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "- #{id} · {wing}/{room} · {preview}\n",
            id = d.id,
            wing = d.wing,
            preview = d.body_preview,
        ));
    }
    if resp.drawers.len() > MAX_DRAWERS {
        out.push_str(&format!(
            "  …and {} more drawers (truncated)\n",
            resp.drawers.len() - MAX_DRAWERS,
        ));
    }
    out
}

/// `ResourceProvider` that reads the cached snapshot. Constructed
/// from an `Arc<RwLock<ResourceSnapshot>>` shared with the
/// AppState writer; reads on every `snapshot(scope)` call are
/// cheap (RwLock read + clone of the underlying strings).
pub struct AppStateResourceProvider {
    inner: Arc<RwLock<ResourceSnapshot>>,
}

impl AppStateResourceProvider {
    pub fn new(inner: Arc<RwLock<ResourceSnapshot>>) -> Self {
        Self { inner }
    }
}

impl ResourceProvider for AppStateResourceProvider {
    fn snapshot(&self, _scope: IncludeContext) -> Vec<ResourceSnippet> {
        // `_scope` ignored for v1 — this provider only knows about
        // *this* desktop. AllServers will matter once we start
        // proxying to external MCP servers (the host role per
        // `MCP-NATIVE.md` §4.2), at which point we expand here.
        let snap = match self.inner.read() {
            Ok(s) => s,
            // Poisoned lock — return empty rather than panic.
            // Instrumentation must never break the call path.
            Err(_) => return Vec::new(),
        };
        if snap.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(3);
        if !snap.agents_text.is_empty() {
            out.push(ResourceSnippet {
                uri: "desktop://agents".to_string(),
                content: snap.agents_text.clone(),
            });
        }
        if !snap.activity_text.is_empty() {
            out.push(ResourceSnippet {
                uri: "desktop://activity/recent".to_string(),
                content: snap.activity_text.clone(),
            });
        }
        if !snap.memory_text.is_empty() {
            out.push(ResourceSnippet {
                uri: "desktop://memory/recent".to_string(),
                content: snap.memory_text.clone(),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_returns_no_snippets() {
        let inner = Arc::new(RwLock::new(ResourceSnapshot::default()));
        let p = AppStateResourceProvider::new(inner);
        let snippets = p.snapshot(IncludeContext::ThisServer);
        assert!(snippets.is_empty());
    }

    #[test]
    fn snapshot_with_only_agents_returns_one_snippet() {
        let mut s = ResourceSnapshot::default();
        s.agents_text = "- agent-42 · running · runtime=bullmq · model=qwen3.5\n".into();
        let inner = Arc::new(RwLock::new(s));
        let p = AppStateResourceProvider::new(inner);
        let snippets = p.snapshot(IncludeContext::ThisServer);
        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].uri, "desktop://agents");
        assert!(snippets[0].content.contains("agent-42"));
    }

    #[test]
    fn snapshot_with_all_three_sections_returns_three_snippets() {
        let s = ResourceSnapshot {
            agents_text: "agents".into(),
            activity_text: "activity".into(),
            memory_text: "memory".into(),
        };
        let inner = Arc::new(RwLock::new(s));
        let p = AppStateResourceProvider::new(inner);
        let snippets = p.snapshot(IncludeContext::AllServers);
        let uris: Vec<_> = snippets.iter().map(|s| s.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                "desktop://agents",
                "desktop://activity/recent",
                "desktop://memory/recent",
            ],
        );
    }

    #[test]
    fn snapshot_skips_sections_with_empty_text() {
        // Only memory populated → only one snippet emitted, with
        // the memory URI. Order-independent test for the gating
        // logic.
        let s = ResourceSnapshot {
            agents_text: String::new(),
            activity_text: String::new(),
            memory_text: "drawers".into(),
        };
        let inner = Arc::new(RwLock::new(s));
        let p = AppStateResourceProvider::new(inner);
        let snippets = p.snapshot(IncludeContext::ThisServer);
        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].uri, "desktop://memory/recent");
    }
}
