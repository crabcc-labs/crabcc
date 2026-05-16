//! MCP sampling-offer — `sampling/createMessage` proxied through
//! the local LiteLLM stack. Spec at
//! `crates/crabcc-desktop/docs/MCP-SAMPLING-OFFER.md`.
//!
//! M3-first cut. This module implements the *core* sampling logic
//! (request validation, depth cap, model selection, parameter
//! mapping, LiteLLM proxy call, response translation) as a
//! transport-agnostic [`SamplingHandler`] trait. A follow-up slice
//! wires it into an MCP server handshake on whichever transport
//! we end up running (Unix socket for `BullmqRuntime` containers;
//! Telegram bridge for the iPhone path).
//!
//! Deferred from this slice (per `MCP-SAMPLING-OFFER.md` §11):
//! `includeContext` summarisation, response streaming via
//! progress notifications, the consent UI toast (handler hardcodes
//! `allow-trusted`-for-localhost). Each lands in its own follow-up.
//!
//! Synchronous surface — the desktop crate uses `reqwest::blocking`
//! and shells everything heavy onto background threads via `flume`.
//! Callers in the route layer must spawn before invoking
//! [`SamplingHandler::handle`] so the gpui render thread doesn't
//! stall on a 30-second qwen3.5 call.

use serde::{Deserialize, Serialize};

/// Default LiteLLM proxy endpoint per
/// `install/ollama-stack/docker-compose.yml`. Override via the
/// `LITELLM_ENDPOINT` env var picked up by [`LiteLlmSamplingHandler::from_env`].
pub const DEFAULT_LITELLM_ENDPOINT: &str = "http://127.0.0.1:4000/v1/chat/completions";

/// Hard cap on `_meta.samplingDepth`. Per spec §12.1 — reject
/// inbound requests at depth >= 3 with [`SamplingErrorKind::DepthExceeded`].
pub const MAX_SAMPLING_DEPTH: u8 = 3;

/// Hardware floor for the qwen3.5:35b primary model. Hosts below
/// this fall back to smaller models or return
/// [`SamplingErrorKind::NoSuitableModel`]. Per
/// `reference_ollama_mlx.md`.
pub const QWEN35_RAM_FLOOR_GB: u32 = 32;

/// Default summary lane — small Apple-Silicon-friendly Qwen3 used
/// when `includeContext` is set and the host needs to compress
/// resource snippets before stuffing them into the primary
/// request. Mirrors the `ollama/qwen3:4b` entry in
/// `install/ollama-stack/litellm.config.yaml`. See
/// `MCP-SAMPLING-OFFER.md` §7.1.
pub const DEFAULT_SUMMARY_MODEL: &str = "ollama/qwen3:4b";

/// Cap on summary-input character count. Snippets concatenated
/// past this are truncated before the summary call so a runaway
/// resource provider can't burn 30 s of inference time on a 100 MB
/// log tail. Picked at 32 KB (~8k tokens) as a conservative cap
/// that leaves the qwen3:4b context window well under saturation.
pub const SUMMARY_INPUT_CAP_BYTES: usize = 32 * 1024;

/// Reserved JSON-RPC error codes we return to peers.
/// Spec `MCP-SAMPLING-OFFER.md` §10.
mod error_codes {
    pub const SAMPLING_DENIED: i32 = -32001;
    pub const SAMPLING_UNAVAILABLE: i32 = -32002;
    pub const MODEL_NOT_LOADED: i32 = -32003;
    pub const RATE_LIMITED: i32 = -32004;
    pub const DEPTH_EXCEEDED: i32 = -32005;
    pub const NO_SUITABLE_MODEL: i32 = -32006;
}

// ───────────────────────────────────────── request/response types

/// Inbound `sampling/createMessage` params. Mirrors MCP's standard
/// shape but with `_meta.samplingDepth` carried explicitly so the
/// handler can enforce the depth cap (§12.1).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SamplingRequest {
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<ModelPreferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// MCP `includeContext` selector. When set, the handler asks
    /// its [`ResourceProvider`] for a snapshot, summarises with the
    /// secondary lane (`qwen3:4b` by default), and prepends the
    /// summary to the primary request as a system-level context
    /// block. See `MCP-SAMPLING-OFFER.md` §3.2 / §7.1.
    #[serde(
        default,
        rename = "includeContext",
        skip_serializing_if = "Option::is_none"
    )]
    pub include_context: Option<IncludeContext>,
    /// MCP `_meta` field. We only read `samplingDepth` from it for now.
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<SamplingMeta>,
}

/// MCP `includeContext` value. `None` = no context injection;
/// `ThisServer` = snapshot just the directly-connected server's
/// resources; `AllServers` = snapshot every connected server's.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IncludeContext {
    None,
    ThisServer,
    AllServers,
}

impl IncludeContext {
    /// `None` (or absent) means "do not inject" — exposed so
    /// callers don't have to match the variant inline.
    pub fn injects(self) -> bool {
        !matches!(self, IncludeContext::None)
    }
}

/// One resource snippet to feed the summary lane. The handler
/// concatenates these and asks the summary model to compress them
/// into a single short context block.
#[derive(Debug, Clone)]
pub struct ResourceSnippet {
    pub uri: String,
    pub content: String,
}

/// Source of resource snippets for `includeContext`. Implementors
/// know which servers / rooms the host is connected to and return
/// the relevant subset for the requested scope.
///
/// Calls happen on the sampling-handler's worker thread, so impls
/// must be `Send + Sync`. Implementations should be cheap and
/// non-blocking — a slow provider stalls the whole sampling call.
pub trait ResourceProvider: Send + Sync {
    fn snapshot(&self, scope: IncludeContext) -> Vec<ResourceSnippet>;
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SamplingMeta {
    /// 0 at the root request; the handler increments this when
    /// nesting (today: never; reserved for sampling-of-sampling
    /// follow-up). Peers that re-enter us carry the parent depth
    /// so the cap survives the chain.
    #[serde(default, rename = "samplingDepth")]
    pub sampling_depth: u8,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Content,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text { text: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ModelPreferences {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<ModelHint>,
    /// 0..1, larger = prefer cheaper.
    #[serde(
        default,
        rename = "costPriority",
        skip_serializing_if = "Option::is_none"
    )]
    pub cost_priority: Option<f32>,
    #[serde(
        default,
        rename = "speedPriority",
        skip_serializing_if = "Option::is_none"
    )]
    pub speed_priority: Option<f32>,
    #[serde(
        default,
        rename = "intelligencePriority",
        skip_serializing_if = "Option::is_none"
    )]
    pub intelligence_priority: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelHint {
    pub name: String,
}

/// Outbound `sampling/createMessage` result.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SamplingResponse {
    pub role: Role,
    pub content: Content,
    pub model: String,
    #[serde(rename = "stopReason")]
    pub stop_reason: FinishReason,
    /// Token counts from the upstream provider when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FinishReason {
    EndTurn,
    StopSequence,
    MaxTokens,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Usage {
    #[serde(default, rename = "promptTokens")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, rename = "completionTokens")]
    pub completion_tokens: Option<u32>,
}

// ───────────────────────────────────────── errors

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingErrorKind {
    Denied,
    Unavailable,
    ModelNotLoaded,
    RateLimited,
    DepthExceeded,
    NoSuitableModel,
}

impl SamplingErrorKind {
    pub fn code(self) -> i32 {
        match self {
            SamplingErrorKind::Denied => error_codes::SAMPLING_DENIED,
            SamplingErrorKind::Unavailable => error_codes::SAMPLING_UNAVAILABLE,
            SamplingErrorKind::ModelNotLoaded => error_codes::MODEL_NOT_LOADED,
            SamplingErrorKind::RateLimited => error_codes::RATE_LIMITED,
            SamplingErrorKind::DepthExceeded => error_codes::DEPTH_EXCEEDED,
            SamplingErrorKind::NoSuitableModel => error_codes::NO_SUITABLE_MODEL,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SamplingErrorKind::Denied => "sampling_denied",
            SamplingErrorKind::Unavailable => "sampling_unavailable",
            SamplingErrorKind::ModelNotLoaded => "model_not_loaded",
            SamplingErrorKind::RateLimited => "rate_limited",
            SamplingErrorKind::DepthExceeded => "sampling_depth_exceeded",
            SamplingErrorKind::NoSuitableModel => "no_suitable_model",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SamplingError {
    pub kind: SamplingErrorKind,
    pub message: String,
}

impl SamplingError {
    pub fn new(kind: SamplingErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SamplingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.label(), self.message)
    }
}

impl std::error::Error for SamplingError {}

// ───────────────────────────────────────── handler trait

/// Synchronous handler for `sampling/createMessage`. Implementors
/// deliver an LLM completion or a typed [`SamplingError`].
///
/// Synchronous because the rest of the desktop crate's HTTP work
/// uses `reqwest::blocking`; the route layer offloads onto
/// background threads via `flume` channels (mirrors the existing
/// `submit_*` pattern in `state.rs`).
pub trait SamplingHandler: Send + Sync {
    fn handle(&self, request: SamplingRequest) -> Result<SamplingResponse, SamplingError>;
}

/// Side-channel observer for sampling lifecycle events. Lets the
/// inspector ring (or any future audit sink) record start/end
/// pairs without coupling [`SamplingHandler`] to the inspector
/// types.
///
/// The handler calls `on_request` immediately after model
/// selection and `on_response` immediately after the upstream
/// call returns. A request id minted in `on_request` is threaded
/// through to `on_response` so the consumer can link the two
/// events (parent_id-style).
///
/// **Sub-calls.** When the handler issues a nested call internally
/// — today, the `includeContext` summary lane firing
/// `qwen3:4b` to compress resource snippets — it notifies via
/// `on_subcall_request` / `on_subcall_response`. The `parent_id`
/// is the id returned from the top-level `on_request`. Default
/// impls are no-ops so observers that don't care about subcalls
/// see no change in behaviour.
pub trait SamplingObserver: Send + Sync {
    /// Returns a u64 token that identifies this in-flight request.
    /// Free-form — observers that don't care can return 0.
    fn on_request(&self, request: &SamplingRequest, chosen_model: &str) -> u64;
    fn on_response(
        &self,
        request_id: u64,
        result: &Result<SamplingResponse, SamplingError>,
        latency_ms: u32,
    );

    /// Called when the handler is about to fire a nested sampling
    /// call as part of fulfilling the parent. The classic case is
    /// the `includeContext` summary lane (parent at depth N invokes
    /// a child at depth N+1 to compress resource snippets).
    ///
    /// `method` is the MCP method name (today always
    /// `"sampling/createMessage"`; reserved for future expansion).
    /// `chosen_model` is the model resolved for the *sub*-call,
    /// not the parent.
    ///
    /// Returns a sub-id which the matching `on_subcall_response`
    /// receives. Free-form like the top-level id.
    ///
    /// Default impl: no-op, returns 0.
    fn on_subcall_request(
        &self,
        parent_id: u64,
        method: &str,
        chosen_model: &str,
        request: &SamplingRequest,
    ) -> u64 {
        let _ = (parent_id, method, chosen_model, request);
        0
    }

    /// Called after the matching nested call returns. `parent_id`
    /// echoes the top-level parent so consumers can render the
    /// chain even if they didn't track `sub_id` mapping themselves.
    ///
    /// Default impl: no-op.
    fn on_subcall_response(
        &self,
        sub_id: u64,
        parent_id: u64,
        result: &Result<SamplingResponse, SamplingError>,
        latency_ms: u32,
    ) {
        let _ = (sub_id, parent_id, result, latency_ms);
    }
}
