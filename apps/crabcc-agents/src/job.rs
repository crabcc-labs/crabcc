use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Which agent CLI runs inside the container. The image bundles both;
/// dispatch happens in `agent-runner/entrypoint.sh` based on `AGENT_KIND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// Claude Code — full-featured agent with --sandbox, MCP tools,
    /// SSO auth pass-through. The default.
    #[default]
    ClaudeCode,
    /// mini-swe-agent (https://mini-swe-agent.com) — minimal SWE agent
    /// (~100 LoC, Python). Useful for narrow "fix this failing test"
    /// jobs where the heavy Claude Code surface is overkill.
    MiniSwe,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::MiniSwe => "mini-swe",
        }
    }
}

/// Payload pushed by producers (Telegram bot, dashboard, MCP, …).
///
/// Producers are TS/JS-friendly: the payload is the full BullMQ
/// `job.data` blob. The Rust worker decodes it as `AgentJob`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJob {
    /// Free-form prompt / task description handed to the agent.
    pub prompt: String,

    /// Which agent CLI to invoke. Defaults to Claude Code.
    #[serde(default)]
    pub kind: AgentKind,

    /// Optional model override. Defaults to `AGENT_DEFAULT_MODEL`
    /// (service-level: `claude-sonnet-4-6`).
    #[serde(default)]
    pub model: Option<String>,

    /// Optional reasoning-effort override: `high` | `medium` | `low`.
    /// Defaults to `AGENT_DEFAULT_EFFORT` (service-level: `high`).
    /// Mapped to a `--append-system-prompt` directive at run time.
    #[serde(default)]
    pub effort: Option<String>,

    /// Sandbox toggles. Maps onto Claude Code's `--sandbox` flags
    /// (https://code.claude.com/docs/en/sandboxing). Defaults to a
    /// strict sandbox (network off, fs read-only outside /workspace).
    #[serde(default)]
    pub sandbox: SandboxSpec,

    /// Extra env vars passed into the agent container. Producers MUST
    /// NOT include host secrets here unless explicitly intended.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Optional per-job timeout override (seconds). Capped by service
    /// config `AGENT_TIMEOUT_SECS`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Trackability headers — caller-supplied key/value pairs that
    /// propagate through the whole pipeline:
    ///   • container env as `CRABCC_HEADER_<UPPERSNAKE_KEY>=<value>`
    ///     (so axint, claude-code, mini-swe-agent CLIs can forward
    ///     them onward via litellm's `forward_client_headers_to_llm_api`),
    ///   • a single `s=event m=headers <json>` entry at the head of
    ///     the per-job Redis Stream so consumers (live-web dashboard,
    ///     PR-bot, telegram bot) can correlate without parsing each line,
    ///   • tracing spans on the worker side.
    ///
    /// Convention: lower-case, `x-`-prefixed keys per HTTP custom-header
    /// idiom. Common keys:
    ///   x-source        — "cli" | "telegram" | "live-web" | "pr-bot"
    ///   x-request-id    — caller-supplied UUID for tracing
    ///   x-trace-id      — OTel trace id
    ///   x-job-run-id    — correlates to ~/.crabcc/agents/<id>/
    ///
    /// Avoid PII: do NOT put usernames, emails, or session ids here —
    /// the headers ride along through Redis logs, container env, and
    /// upstream LLM logs, none of which are PII-safe surfaces.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SandboxSpec {
    /// Allow outbound network. Default off.
    #[serde(default)]
    pub network: bool,

    /// Allow filesystem writes outside the tmpfs workspace mount.
    /// Default off.
    #[serde(default)]
    pub writeable_root: bool,

    /// Allow Claude Code to invoke arbitrary Bash. Default on (this
    /// is the whole point of agentic runs); set false to restrict to
    /// MCP-only tool surface.
    #[serde(default = "default_true")]
    pub bash: bool,
}

fn default_true() -> bool {
    true
}
