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
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default LiteLLM proxy endpoint per
/// `install/ollama-stack/docker-compose.yml`. Override via
/// [`LiteLlmSamplingHandler::with_endpoint`].
pub const DEFAULT_LITELLM_ENDPOINT: &str = "http://127.0.0.1:4000/v1/chat/completions";

/// Hard cap on `_meta.samplingDepth`. Per spec §12.1 — reject
/// inbound requests at depth >= 3 with [`SamplingErrorKind::DepthExceeded`].
pub const MAX_SAMPLING_DEPTH: u8 = 3;

/// Hardware floor for the qwen3.5:35b primary model. Hosts below
/// this fall back to smaller models or return
/// [`SamplingErrorKind::NoSuitableModel`]. Per
/// `reference_ollama_mlx.md`.
pub const QWEN35_RAM_FLOOR_GB: u32 = 32;

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
    /// MCP `_meta` field. We only read `samplingDepth` from it for now.
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<SamplingMeta>,
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
    #[serde(default, rename = "costPriority", skip_serializing_if = "Option::is_none")]
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
}

// ───────────────────────────────────────── LiteLLM impl

/// Production handler. Proxies to the LiteLLM container fronted by
/// `install/ollama-stack/docker-compose.yml`. Knows about the
/// Apple-Silicon-tuned model lineup and the qwen3.5-35b RAM floor.
pub struct LiteLlmSamplingHandler {
    endpoint: String,
    /// Bearer token sent as `Authorization: Bearer …`. Pulled from
    /// `LITELLM_MASTER_KEY` at construct time.
    master_key: String,
    /// Model lineup we'll consider for hint-matching and scoring.
    /// Order is preference order — earlier wins ties.
    catalog: Vec<ModelEntry>,
    /// Cached at startup (read once from sysctl/sysinfo).
    host_ram_gb: u32,
    /// HTTP client — reused across calls for keepalive.
    client: reqwest::blocking::Client,
    /// Optional lifecycle hook. None = no instrumentation. Wired
    /// to `crate::inspector::InspectorSamplingObserver` in
    /// production so every sampling round-trip surfaces in the
    /// inspector ring.
    observer: Option<Arc<dyn SamplingObserver>>,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub name: String,
    /// Whether this is a local-only model (Ollama-backed) or a
    /// cloud-routed one (Anthropic). Drives cost-priority scoring.
    pub local: bool,
    /// Required minimum host RAM (GB) for this model to be served
    /// from local hardware. `None` for cloud models.
    pub min_ram_gb: Option<u32>,
}

impl ModelEntry {
    pub fn local(name: &str, min_ram_gb: u32) -> Self {
        Self {
            name: name.to_string(),
            local: true,
            min_ram_gb: Some(min_ram_gb),
        }
    }
    pub fn cloud(name: &str) -> Self {
        Self {
            name: name.to_string(),
            local: false,
            min_ram_gb: None,
        }
    }
}

/// Mirror of the LiteLLM/Ollama-stack lineup the host runs. Keeping
/// it in code avoids a runtime fetch of the LiteLLM `/v1/models`
/// endpoint on every cold call. If the user edits the YAML config,
/// they'll need to keep this list in sync — small price for not
/// adding a startup HTTP roundtrip into the route render path.
pub fn default_catalog() -> Vec<ModelEntry> {
    vec![
        ModelEntry::local("qwen3.5:35b-a3b-coding-nvfp4", QWEN35_RAM_FLOOR_GB),
        ModelEntry::local("qwen3:4b", 8),
        ModelEntry::local("qwen2.5-coder", 8),
        ModelEntry::cloud("claude-sonnet-4-6"),
        ModelEntry::cloud("claude-haiku-4-5"),
        ModelEntry::cloud("claude-opus-4-7"),
    ]
}

impl LiteLlmSamplingHandler {
    /// Build a handler with the default catalog + endpoint, reading
    /// the master key from `LITELLM_MASTER_KEY`. Returns
    /// [`SamplingErrorKind::Unavailable`] if the env var is absent.
    pub fn from_env() -> Result<Self, SamplingError> {
        let master_key = std::env::var("LITELLM_MASTER_KEY").map_err(|_| {
            SamplingError::new(
                SamplingErrorKind::Unavailable,
                "LITELLM_MASTER_KEY env var not set",
            )
        })?;
        let host_ram_gb = detect_host_ram_gb().unwrap_or(0);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| {
                SamplingError::new(
                    SamplingErrorKind::Unavailable,
                    format!("reqwest client build: {e}"),
                )
            })?;
        Ok(Self {
            endpoint: DEFAULT_LITELLM_ENDPOINT.to_string(),
            master_key,
            catalog: default_catalog(),
            host_ram_gb,
            client,
            observer: None,
        })
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn with_catalog(mut self, catalog: Vec<ModelEntry>) -> Self {
        self.catalog = catalog;
        self
    }

    pub fn with_host_ram_gb(mut self, gb: u32) -> Self {
        self.host_ram_gb = gb;
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn SamplingObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Pure model selection — exposed as a free fn for unit testing.
    fn select_model(&self, prefs: Option<&ModelPreferences>) -> Result<&ModelEntry, SamplingError> {
        select_model(&self.catalog, self.host_ram_gb, prefs)
    }
}

impl SamplingHandler for LiteLlmSamplingHandler {
    fn handle(&self, request: SamplingRequest) -> Result<SamplingResponse, SamplingError> {
        // Depth gate first — reject before doing any work.
        let depth = request.meta.as_ref().map(|m| m.sampling_depth).unwrap_or(0);
        if depth >= MAX_SAMPLING_DEPTH {
            return Err(SamplingError::new(
                SamplingErrorKind::DepthExceeded,
                format!("samplingDepth {depth} >= cap {MAX_SAMPLING_DEPTH}"),
            ));
        }

        let model = self.select_model(request.model_preferences.as_ref())?.clone();

        // Notify observer that a request is in flight. Errors from
        // the observer are not propagated — instrumentation must
        // never break the call.
        let req_id = self
            .observer
            .as_ref()
            .map(|o| o.on_request(&request, &model.name));
        let started = Instant::now();

        let result = self.do_call(&model.name, &request);

        if let (Some(o), Some(id)) = (self.observer.as_ref(), req_id) {
            let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
            o.on_response(id, &result, latency_ms);
        }
        result
    }
}

impl LiteLlmSamplingHandler {
    /// Pure I/O bit, factored out so [`SamplingHandler::handle`]
    /// can wrap it with observer notifications without nesting
    /// match arms.
    fn do_call(
        &self,
        model: &str,
        request: &SamplingRequest,
    ) -> Result<SamplingResponse, SamplingError> {
        let body = build_openai_request(model, request);
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.master_key)
            .json(&body)
            .send()
            .map_err(|e| {
                SamplingError::new(
                    SamplingErrorKind::Unavailable,
                    format!("LiteLLM request failed: {e}"),
                )
            })?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(SamplingError::new(
                SamplingErrorKind::RateLimited,
                format!("upstream returned {status}"),
            ));
        }
        if !status.is_success() {
            return Err(SamplingError::new(
                SamplingErrorKind::Unavailable,
                format!("upstream returned {status}"),
            ));
        }

        let oa: OpenAiResponse = resp.json().map_err(|e| {
            SamplingError::new(
                SamplingErrorKind::Unavailable,
                format!("decoding LiteLLM response: {e}"),
            )
        })?;

        translate_openai_response(model, oa)
    }
}

// ───────────────────────────────────────── pure helpers (testable)

/// Score-and-pick a model from `catalog` given the user's preferences.
/// Algorithm per `MCP-SAMPLING-OFFER.md` §6.
pub fn select_model<'a>(
    catalog: &'a [ModelEntry],
    host_ram_gb: u32,
    prefs: Option<&ModelPreferences>,
) -> Result<&'a ModelEntry, SamplingError> {
    let prefs = prefs.cloned().unwrap_or_default();

    // 1. Hint match — first hint name that's a prefix of any
    //    catalog entry wins.
    for hint in &prefs.hints {
        if let Some(entry) = catalog
            .iter()
            .find(|e| e.name.starts_with(&hint.name) && fits_host(e, host_ram_gb))
        {
            return Ok(entry);
        }
    }

    // 2. Priority weighting — score each candidate the host can run.
    let cost = prefs.cost_priority.unwrap_or(0.5);
    let intel = prefs.intelligence_priority.unwrap_or(0.5);
    // speed isn't yet differentiated in the catalog; reserved for
    // future per-model latency hints.
    let _ = prefs.speed_priority;

    let mut best: Option<(&ModelEntry, f32)> = None;
    for entry in catalog.iter().filter(|e| fits_host(e, host_ram_gb)) {
        // Local models score 1.0 on cost (zero marginal cost),
        // 0.7 on intelligence (35B is good but not Opus). Cloud
        // models score 0.0 on cost, 1.0 on intelligence.
        let local_score = if entry.local { 1.0_f32 } else { 0.0_f32 };
        let intel_score = if entry.local { 0.7_f32 } else { 1.0_f32 };
        let score = cost * local_score + intel * intel_score;
        match &best {
            Some((_, b)) if *b >= score => {}
            _ => best = Some((entry, score)),
        }
    }

    best.map(|(e, _)| e).ok_or_else(|| {
        SamplingError::new(
            SamplingErrorKind::NoSuitableModel,
            format!("no model in catalog fits a {host_ram_gb}GB host"),
        )
    })
}

fn fits_host(entry: &ModelEntry, host_ram_gb: u32) -> bool {
    match entry.min_ram_gb {
        Some(floor) => host_ram_gb >= floor,
        None => true, // cloud models: always available (LiteLLM mediates)
    }
}

/// Translate MCP request → OpenAI/LiteLLM request body.
pub fn build_openai_request(model: &str, request: &SamplingRequest) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len() + 1);
    if let Some(sys) = &request.system_prompt {
        // Merge into existing leading system message if present;
        // otherwise prepend.
        if matches!(request.messages.first().map(|m| m.role), Some(Role::System)) {
            // Caller already prepared a system slot — append our
            // text to it so we don't drop either.
            let first = &request.messages[0];
            let combined = match &first.content {
                Content::Text { text } => format!("{sys}\n\n{text}"),
            };
            messages.push(serde_json::json!({
                "role": "system",
                "content": combined,
            }));
            for m in request.messages.iter().skip(1) {
                messages.push(message_to_openai(m));
            }
        } else {
            messages.push(serde_json::json!({
                "role": "system",
                "content": sys,
            }));
            for m in &request.messages {
                messages.push(message_to_openai(m));
            }
        }
    } else {
        for m in &request.messages {
            messages.push(message_to_openai(m));
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
    });
    let m = body.as_object_mut().expect("body is an object");
    if let Some(t) = request.temperature {
        m.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(mt) = request.max_tokens {
        m.insert("max_tokens".into(), serde_json::json!(mt));
    }
    if !request.stop_sequences.is_empty() {
        m.insert(
            "stop".into(),
            serde_json::json!(request.stop_sequences.clone()),
        );
    }
    body
}

fn message_to_openai(m: &Message) -> serde_json::Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let text = match &m.content {
        Content::Text { text } => text,
    };
    serde_json::json!({
        "role": role,
        "content": text,
    })
}

/// Translate OpenAI/LiteLLM response → MCP-shaped response.
pub fn translate_openai_response(
    requested_model: &str,
    resp: OpenAiResponse,
) -> Result<SamplingResponse, SamplingError> {
    let choice = resp.choices.into_iter().next().ok_or_else(|| {
        SamplingError::new(
            SamplingErrorKind::Unavailable,
            "upstream returned no choices",
        )
    })?;
    let stop_reason = match choice.finish_reason.as_deref() {
        Some("stop") | Some("end_turn") | None => FinishReason::EndTurn,
        Some("length") | Some("max_tokens") => FinishReason::MaxTokens,
        Some("content_filter") | Some("tool_calls") => FinishReason::EndTurn,
        // LiteLLM sometimes surfaces "stop_sequence" verbatim when
        // a stop[] entry triggered the cutoff.
        Some("stop_sequence") => FinishReason::StopSequence,
        Some(_) => FinishReason::EndTurn,
    };
    Ok(SamplingResponse {
        role: Role::Assistant,
        content: Content::Text {
            text: choice.message.content,
        },
        // The upstream may have swapped models via LiteLLM
        // fallbacks; surface its `model` field if present.
        model: resp.model.unwrap_or_else(|| requested_model.to_string()),
        stop_reason,
        usage: resp.usage.map(|u| Usage {
            prompt_tokens: Some(u.prompt_tokens),
            completion_tokens: Some(u.completion_tokens),
        }),
    })
}

// ───────────────────────────────────────── upstream wire types

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiResponse {
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<OpenAiChoice>,
    #[serde(default)]
    pub usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiChoice {
    pub message: OpenAiMessage,
    #[serde(default, rename = "finish_reason")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct OpenAiUsage {
    #[serde(default, rename = "prompt_tokens")]
    pub prompt_tokens: u32,
    #[serde(default, rename = "completion_tokens")]
    pub completion_tokens: u32,
}

// ───────────────────────────────────────── host detection

/// Best-effort RAM detection, used once at startup. Returns `None`
/// on platforms we don't handle, leaving the handler at the
/// fail-safe 0-GB lower bound (which gates *every* local model).
#[cfg(target_os = "macos")]
fn detect_host_ram_gb() -> Option<u32> {
    // sysctl hw.memsize → bytes
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let bytes: u64 = s.trim().parse().ok()?;
    Some((bytes / 1_073_741_824).try_into().ok()?)
}

#[cfg(not(target_os = "macos"))]
fn detect_host_ram_gb() -> Option<u32> {
    // Linux / Windows: punt for now — the host of record for v1 is
    // macOS. Once we have a Linux story for the desktop crate, port
    // /proc/meminfo parsing here.
    None
}

// ───────────────────────────────────────── tests

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: Content::Text {
                text: text.to_string(),
            },
        }
    }

    fn req(meta_depth: u8, hint: Option<&str>) -> SamplingRequest {
        SamplingRequest {
            messages: vec![user("hello")],
            model_preferences: hint.map(|h| ModelPreferences {
                hints: vec![ModelHint {
                    name: h.to_string(),
                }],
                ..Default::default()
            }),
            system_prompt: None,
            max_tokens: Some(128),
            stop_sequences: vec![],
            temperature: Some(0.2),
            meta: Some(SamplingMeta {
                sampling_depth: meta_depth,
            }),
        }
    }

    #[test]
    fn depth_cap_rejects_at_max() {
        // We can drive depth-cap logic via a custom handler that
        // never reaches the network.
        let handler = LiteLlmSamplingHandler {
            endpoint: "http://invalid.localhost".into(),
            master_key: "x".into(),
            catalog: default_catalog(),
            host_ram_gb: 64,
            client: reqwest::blocking::Client::new(),
            observer: None,
        };
        let r = handler.handle(req(MAX_SAMPLING_DEPTH, None));
        match r {
            Err(e) => assert_eq!(e.kind, SamplingErrorKind::DepthExceeded),
            Ok(_) => panic!("expected DepthExceeded"),
        }
    }

    #[test]
    fn depth_cap_accepts_below_max() {
        // Depth check happens BEFORE network — so a depth-2 request
        // fails on the network call (Unavailable), not DepthExceeded.
        let handler = LiteLlmSamplingHandler {
            endpoint: "http://127.0.0.1:1/never".into(),
            master_key: "x".into(),
            catalog: default_catalog(),
            host_ram_gb: 64,
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(50))
                .build()
                .unwrap(),
            observer: None,
        };
        let r = handler.handle(req(MAX_SAMPLING_DEPTH - 1, Some("qwen3.5")));
        match r {
            Err(e) => assert_eq!(
                e.kind,
                SamplingErrorKind::Unavailable,
                "expected upstream-unavailable, got {e}"
            ),
            Ok(_) => panic!("network call should have failed"),
        }
    }

    #[test]
    fn select_model_hint_match_picks_first_prefix() {
        let cat = default_catalog();
        let prefs = ModelPreferences {
            hints: vec![ModelHint {
                name: "qwen3.5".into(),
            }],
            ..Default::default()
        };
        let m = select_model(&cat, 64, Some(&prefs)).unwrap();
        assert_eq!(m.name, "qwen3.5:35b-a3b-coding-nvfp4");
    }

    #[test]
    fn select_model_hint_falls_through_below_ram_floor() {
        // 16GB host can't run the 35B; prefix-match still tries it
        // first but `fits_host` filters it out → next candidate.
        let cat = default_catalog();
        let prefs = ModelPreferences {
            hints: vec![ModelHint {
                name: "qwen3".into(),
            }],
            ..Default::default()
        };
        let m = select_model(&cat, 16, Some(&prefs)).unwrap();
        // qwen3:4b has min_ram_gb = 8; qwen3.5-35b has 32 (filtered out).
        assert_eq!(m.name, "qwen3:4b");
    }

    #[test]
    fn select_model_high_cost_priority_picks_local() {
        let cat = default_catalog();
        let prefs = ModelPreferences {
            cost_priority: Some(1.0),
            intelligence_priority: Some(0.0),
            ..Default::default()
        };
        let m = select_model(&cat, 64, Some(&prefs)).unwrap();
        assert!(
            m.local,
            "high cost priority should pick a local model; got {}",
            m.name
        );
    }

    #[test]
    fn select_model_high_intel_priority_picks_cloud() {
        let cat = default_catalog();
        let prefs = ModelPreferences {
            cost_priority: Some(0.0),
            intelligence_priority: Some(1.0),
            ..Default::default()
        };
        let m = select_model(&cat, 64, Some(&prefs)).unwrap();
        assert!(
            !m.local,
            "high intelligence priority should pick a cloud model; got {}",
            m.name
        );
    }

    #[test]
    fn select_model_zero_ram_returns_no_suitable_for_local_only() {
        let cat = vec![ModelEntry::local("qwen3:4b", 8)];
        let r = select_model(&cat, 0, None);
        match r {
            Err(e) => assert_eq!(e.kind, SamplingErrorKind::NoSuitableModel),
            Ok(_) => panic!("expected NoSuitableModel"),
        }
    }

    #[test]
    fn build_openai_request_prepends_system_prompt() {
        let r = SamplingRequest {
            messages: vec![user("hi")],
            model_preferences: None,
            system_prompt: Some("you are a code reviewer".into()),
            max_tokens: Some(64),
            stop_sequences: vec!["</think>".into()],
            // 0.5 instead of 0.1 because 0.1_f32 → JSON roundtrips
            // as 0.10000000149011612, which doesn't compare equal to
            // the 0.1_f64 in the assertion. 0.5 is exact in both.
            temperature: Some(0.5),
            meta: None,
        };
        let body = build_openai_request("ollama/qwen3:4b", &r);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "you are a code reviewer");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hi");
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["stop"][0], "</think>");
        assert_eq!(body["temperature"], 0.5);
    }

    #[test]
    fn build_openai_request_merges_into_existing_system_slot() {
        let r = SamplingRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: Content::Text {
                        text: "you are terse".into(),
                    },
                },
                user("hi"),
            ],
            model_preferences: None,
            system_prompt: Some("plus: no markdown".into()),
            max_tokens: None,
            stop_sequences: vec![],
            temperature: None,
            meta: None,
        };
        let body = build_openai_request("any", &r);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        let sys = messages[0]["content"].as_str().unwrap();
        assert!(sys.contains("plus: no markdown"));
        assert!(sys.contains("you are terse"));
    }

    #[test]
    fn translate_openai_response_maps_finish_reasons() {
        let mk = |reason: &str| OpenAiResponse {
            model: Some("ollama/qwen3:4b".into()),
            choices: vec![OpenAiChoice {
                message: OpenAiMessage {
                    role: "assistant".into(),
                    content: "ok".into(),
                },
                finish_reason: Some(reason.into()),
            }],
            usage: Some(OpenAiUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
            }),
        };
        let pairs = [
            ("stop", FinishReason::EndTurn),
            ("length", FinishReason::MaxTokens),
            ("max_tokens", FinishReason::MaxTokens),
            ("stop_sequence", FinishReason::StopSequence),
        ];
        for (input, expected) in pairs {
            let out = translate_openai_response("requested", mk(input)).unwrap();
            assert_eq!(out.stop_reason, expected, "for {input}");
            assert_eq!(out.model, "ollama/qwen3:4b");
            assert_eq!(out.usage.unwrap().prompt_tokens, Some(10));
        }
    }

    #[test]
    fn translate_openai_response_falls_back_to_requested_model_when_missing() {
        let resp = OpenAiResponse {
            model: None,
            choices: vec![OpenAiChoice {
                message: OpenAiMessage {
                    role: "assistant".into(),
                    content: "x".into(),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let out = translate_openai_response("ollama/qwen3:4b", resp).unwrap();
        assert_eq!(out.model, "ollama/qwen3:4b");
    }

    #[test]
    fn translate_openai_response_errors_on_no_choices() {
        let resp = OpenAiResponse {
            model: None,
            choices: vec![],
            usage: None,
        };
        let r = translate_openai_response("any", resp);
        match r {
            Err(e) => assert_eq!(e.kind, SamplingErrorKind::Unavailable),
            Ok(_) => panic!("expected Unavailable"),
        }
    }

    /// Recording observer used to assert lifecycle calls fire in
    /// the expected order on both success and error paths.
    struct CountingObserver {
        events: std::sync::Mutex<Vec<&'static str>>,
    }
    impl SamplingObserver for CountingObserver {
        fn on_request(&self, _r: &SamplingRequest, _model: &str) -> u64 {
            self.events.lock().unwrap().push("req");
            42
        }
        fn on_response(
            &self,
            request_id: u64,
            result: &Result<SamplingResponse, SamplingError>,
            _latency_ms: u32,
        ) {
            assert_eq!(request_id, 42, "request id must thread through");
            self.events
                .lock()
                .unwrap()
                .push(if result.is_ok() { "resp_ok" } else { "resp_err" });
        }
    }

    #[test]
    fn observer_fires_on_request_and_response_for_failed_call() {
        // Network call is unreachable → upstream-Unavailable error,
        // but the observer must still see both lifecycle events.
        let obs = Arc::new(CountingObserver {
            events: std::sync::Mutex::new(vec![]),
        });
        let handler = LiteLlmSamplingHandler {
            endpoint: "http://127.0.0.1:1/never".into(),
            master_key: "x".into(),
            catalog: default_catalog(),
            host_ram_gb: 64,
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(50))
                .build()
                .unwrap(),
            observer: Some(obs.clone()),
        };
        let _ = handler.handle(req(0, Some("qwen3.5")));
        let events = obs.events.lock().unwrap();
        assert_eq!(events.as_slice(), ["req", "resp_err"]);
    }

    #[test]
    fn observer_does_not_fire_when_depth_cap_rejects_early() {
        // Depth cap fires *before* model selection, so the observer
        // never sees on_request — there's no model to attribute.
        let obs = Arc::new(CountingObserver {
            events: std::sync::Mutex::new(vec![]),
        });
        let handler = LiteLlmSamplingHandler {
            endpoint: "http://invalid".into(),
            master_key: "x".into(),
            catalog: default_catalog(),
            host_ram_gb: 64,
            client: reqwest::blocking::Client::new(),
            observer: Some(obs.clone()),
        };
        let r = handler.handle(req(MAX_SAMPLING_DEPTH, None));
        assert!(matches!(
            r,
            Err(SamplingError {
                kind: SamplingErrorKind::DepthExceeded,
                ..
            })
        ));
        assert!(obs.events.lock().unwrap().is_empty());
    }

    #[test]
    fn error_codes_match_spec() {
        // Spec MCP-SAMPLING-OFFER.md §10 reserves these.
        assert_eq!(SamplingErrorKind::Denied.code(), -32001);
        assert_eq!(SamplingErrorKind::Unavailable.code(), -32002);
        assert_eq!(SamplingErrorKind::ModelNotLoaded.code(), -32003);
        assert_eq!(SamplingErrorKind::RateLimited.code(), -32004);
        assert_eq!(SamplingErrorKind::DepthExceeded.code(), -32005);
        // -32006 is our own extension for NoSuitableModel.
        assert_eq!(SamplingErrorKind::NoSuitableModel.code(), -32006);
    }
}
