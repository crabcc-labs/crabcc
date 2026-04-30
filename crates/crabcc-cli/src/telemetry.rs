//! Telemetry init — issues #90 + #86.
//!
//! Single-call init for the workspace tracing pipeline. Called from
//! `main()` exactly once, early. Returns a [`TelemetryGuard`] that the
//! caller must keep alive until shutdown.
//!
//! ## Storage contract
//!
//! Telemetry events are written to:
//!   - **stderr** (human-readable, always-on, non-blocking)
//!   - **`.crabcc/telemetry.jsonl`** (structured JSON-lines, per-repo)
//!   - **OTLP endpoint** (opt-in via `OTEL_EXPORTER_OTLP_ENDPOINT`)
//!
//! Aggregated telemetry data is NEVER written to `_internal.db` or
//! `index.db`. Those databases are for agent-run lifecycle and symbol
//! index data respectively. Keeping telemetry in its own append-only
//! log and forwarding to an OTLP collector (rotel) ensures the SQLite
//! files stay purpose-built and don't bloat with observability churn.
//!
//! ## Layers (in init order)
//!
//! 1. stderr fmt    — always on, filtered by RUST_LOG
//! 2. jsonl file    — always on (if cwd is a crabcc repo), KPI filter
//! 3. OTLP          — opt-in: set OTEL_EXPORTER_OTLP_ENDPOINT
//! 4. Telegram      — opt-in: TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID;
//!    forwards WARN/ERROR + named KPI events only
//!
//! ## KPI events (default info level)
//!
//! | Site | Fields |
//! |---|---|
//! | `crabcc_mcp::dispatch_tool_with` | tool, elapsed_ms, ok|error |
//! | `crabcc_core::graph::*` | edges, nodes, count, duration_ms |
//! | `crabcc_cli::agent::*` | x_request_id, x_timings, cold/warm |

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

// ── guard ─────────────────────────────────────────────────────────────────────

/// Holds all non-blocking writer guards + shutdown handles.
/// Drop = flush everything.
pub struct TelemetryGuard {
    _writer: tracing_appender::non_blocking::WorkerGuard,
    _file_writer: Option<tracing_appender::non_blocking::WorkerGuard>,
    _otlp: Option<OtlpHandle>,
    _telegram: Option<TelegramHandle>,
}

// ── OTLP span forwarder ───────────────────────────────────────────────────────
//
// Posts structured span JSON directly to the OTLP HTTP/JSON endpoint
// without pulling in the full opentelemetry_sdk (whose API surface
// changes frequently between minor versions). This is sufficient for
// our use case: forwarding KPI spans to rotel for the /live panel.
//
// Wire format: POST /v1/traces with OTel JSON body (one ResourceSpan per event).
// Aggregation happens entirely inside rotel — NEVER stored in our DBs.

struct OtlpHandle {
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

struct OtlpLayer {
    tx: tokio::sync::mpsc::Sender<serde_json::Value>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for OtlpLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Only forward KPI events (info level, specific targets).
        let level = *event.metadata().level();
        if level > tracing::Level::INFO {
            return;
        }

        let is_kpi = event.metadata().target().starts_with("crabcc_mcp")
            || event.metadata().target().starts_with("crabcc_core::graph")
            || event.metadata().target().starts_with("crabcc_cli::agent");
        if !is_kpi {
            return;
        }

        let mut collector = FieldCollector::default();
        event.record(&mut collector);

        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        // Minimal OTel JSON span — enough for rotel to display in /live.
        let span = serde_json::json!({
            "resourceSpans": [{
                "resource": {
                    "attributes": [
                        {"key": "service.name",    "value": {"stringValue": "crabcc"}},
                        {"key": "service.version", "value": {"stringValue": env!("CARGO_PKG_VERSION")}},
                    ]
                },
                "scopeSpans": [{
                    "scope": {"name": event.metadata().target()},
                    "spans": [{
                        "name":              event.metadata().name(),
                        "startTimeUnixNano": now_ns.to_string(),
                        "endTimeUnixNano":   now_ns.to_string(),
                        "kind":              1,
                        "attributes": collector.as_otel_attrs(),
                    }]
                }]
            }]
        });

        let _ = self.tx.try_send(span);
    }
}

fn try_init_otlp() -> Option<(OtlpHandle, OtlpLayer)> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;
    if endpoint.is_empty() {
        return None;
    }

    let traces_url = Arc::new(format!("{endpoint}/v1/traces"));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(128);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            // Batch up to 50 spans at 1-second intervals.
            let mut batch: Vec<serde_json::Value> = Vec::with_capacity(50);
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    Some(span) = rx.recv() => {
                        batch.push(span);
                        if batch.len() >= 50 { flush_batch(&client, &traces_url, &mut batch).await; }
                    }
                    _ = interval.tick() => {
                        if !batch.is_empty() { flush_batch(&client, &traces_url, &mut batch).await; }
                    }
                    _ = &mut shutdown_rx => {
                        if !batch.is_empty() { flush_batch(&client, &traces_url, &mut batch).await; }
                        break;
                    }
                    else => break,
                }
            }
        });
    }

    Some((
        OtlpHandle {
            _shutdown: shutdown_tx,
        },
        OtlpLayer { tx },
    ))
}

async fn flush_batch(client: &reqwest::Client, url: &str, batch: &mut Vec<serde_json::Value>) {
    // Merge into a single ExportTraceServiceRequest.
    let body = serde_json::json!({ "resourceSpans": batch.iter()
        .flat_map(|b| b["resourceSpans"].as_array().cloned().unwrap_or_default())
        .collect::<Vec<_>>() });
    let _ = client.post(url).json(&body).send().await;
    batch.clear();
}

// ── Telegram notification layer ────────────────────────────────────────────────

struct TelegramHandle {
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

/// Rate-limit: at most 1 Telegram notification per `MIN_GAP_SECS` per
/// unique target+level combination. This prevents a crash loop or a
/// hot-path WARN from flooding the chat.
const TELEGRAM_MIN_GAP_SECS: u64 = 60;

struct TelegramLayer {
    tx: tokio::sync::mpsc::Sender<String>,
    last_sent: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for TelegramLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();

        // Forward WARN/ERROR unconditionally; forward INFO only for
        // named KPI events (agent completions, graph builds, MCP calls).
        let is_kpi = event.metadata().target().starts_with("crabcc_mcp")
            || event.metadata().target().starts_with("crabcc_core::graph")
            || event.metadata().target().starts_with("crabcc_cli::agent");

        if level > tracing::Level::WARN && !is_kpi {
            return;
        }

        // Rate-limit: dedupe by target+level within TELEGRAM_MIN_GAP_SECS.
        // Best-effort: skip if mutex is poisoned or contended.
        let dedup_key = format!("{}:{}", event.metadata().target(), level);
        if let Ok(mut map) = self.last_sent.try_lock() {
            let now = std::time::Instant::now();
            if let Some(prev) = map.get(&dedup_key) {
                if now.duration_since(*prev).as_secs() < TELEGRAM_MIN_GAP_SECS {
                    return;
                }
            }
            map.insert(dedup_key, now);
        }

        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);

        let emoji = match level {
            tracing::Level::ERROR => "🔴",
            tracing::Level::WARN => "🟡",
            _ => "📊",
        };

        let msg = format!(
            "{emoji} `{level}` *crabcc* — `{target}`\n{fields}",
            target = event.metadata().target(),
            fields = visitor.text,
        );

        // Non-blocking: drop if channel is full (bounded at 64).
        let _ = self.tx.try_send(msg);
    }
}

#[derive(Default)]
struct FieldCollector {
    text: String,
    otel: Vec<(String, serde_json::Value)>,
}

impl FieldCollector {
    fn as_otel_attrs(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.otel
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => serde_json::json!({"stringValue": s}),
                        serde_json::Value::Number(n) => serde_json::json!({"doubleValue": n}),
                        serde_json::Value::Bool(b) => serde_json::json!({"boolValue": b}),
                        other => serde_json::json!({"stringValue": other.to_string()}),
                    };
                    serde_json::json!({"key": k, "value": val})
                })
                .collect(),
        )
    }
}

impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let s = format!("{:?}", value);
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("`{}={}`", field.name(), s));
        self.otel
            .push((field.name().to_owned(), serde_json::Value::String(s)));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("`{}={}`", field.name(), value));
        self.otel.push((
            field.name().to_owned(),
            serde_json::Value::String(value.to_owned()),
        ));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("`{}={}`", field.name(), value));
        self.otel
            .push((field.name().to_owned(), serde_json::json!(value)));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("`{}={}`", field.name(), value));
        self.otel
            .push((field.name().to_owned(), serde_json::json!(value)));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(&format!("`{}={}`", field.name(), value));
        self.otel
            .push((field.name().to_owned(), serde_json::Value::Bool(value)));
    }
}

fn try_init_telegram() -> Option<(TelegramHandle, TelegramLayer)> {
    // EXPLICIT OPT-IN required: set CRABCC_TELEGRAM_NOTIFY=1.
    // Having the token in the environment is not enough — prevents
    // accidental notification spam in CI or shared environments.
    if std::env::var("CRABCC_TELEGRAM_NOTIFY").as_deref() != Ok("1") {
        return None;
    }
    let token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
    let chat_id = std::env::var("TELEGRAM_CHAT_ID").ok()?;
    if token.is_empty() || chat_id.is_empty() {
        return None;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let token2 = Arc::new(token);
    let chat_id2 = Arc::new(chat_id);

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            loop {
                tokio::select! {
                    Some(msg) = rx.recv() => {
                        let url = format!(
                            "https://api.telegram.org/bot{}/sendMessage",
                            token2
                        );
                        let _ = client.post(&url)
                            .json(&serde_json::json!({
                                "chat_id":    chat_id2.as_str(),
                                "text":       msg,
                                "parse_mode": "Markdown",
                            }))
                            .send()
                            .await;
                    }
                    _ = &mut shutdown_rx => break,
                    else => break,
                }
            }
        });
    }

    Some((
        TelegramHandle {
            _shutdown: shutdown_tx,
        },
        TelegramLayer {
            tx,
            last_sent: std::sync::Mutex::new(std::collections::HashMap::new()),
        },
    ))
}

// ── init ──────────────────────────────────────────────────────────────────────

/// Initialize the workspace tracing pipeline.
/// Call once, early in main(), before any tracing:: macros fire.
pub fn init() -> TelemetryGuard {
    let (writer, guard) = tracing_appender::non_blocking(io::stderr());

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("crabcc_mcp=info,crabcc_core::graph=info,warn"));

    let fmt_layer = fmt::layer()
        .with_writer(writer)
        .with_target(true)
        .with_ansi(io::stderr().is_terminal())
        .with_filter(filter);

    // JSON file layer — KPI events always captured regardless of RUST_LOG.
    // Stored in .crabcc/telemetry.jsonl (NOT _internal.db / index.db).
    let (file_layer, file_guard) = match telemetry_file_writer() {
        Some((_path, w, g)) => {
            let file_filter =
                EnvFilter::try_from_env("CRABCC_TELEMETRY_LOG").unwrap_or_else(|_| {
                    EnvFilter::new("crabcc_mcp=info,crabcc_core::graph=info,crabcc_cli::agent=info")
                });
            let layer = fmt::layer()
                .json()
                .with_writer(w)
                .with_target(true)
                .with_current_span(false)
                .with_span_list(false)
                .with_filter(file_filter);
            (Some(layer), Some(g))
        }
        None => (None, None),
    };

    // OTLP — aggregation happens in rotel/collector, NOT in our DBs.
    let (otlp_handle, otlp_layer) = match try_init_otlp() {
        Some((h, l)) => (Some(h), Some(l)),
        None => (None, None),
    };

    // Telegram — WARN/ERROR + KPI events forwarded to chat.
    let (tg_handle, tg_layer) = match try_init_telegram() {
        Some((h, l)) => (Some(h), Some(l)),
        None => (None, None),
    };

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(file_layer)
        .with(otlp_layer)
        .with(tg_layer)
        .try_init();

    TelemetryGuard {
        _writer: guard,
        _file_writer: file_guard,
        _otlp: otlp_handle,
        _telegram: tg_handle,
    }
}

/// Resolve `<cwd>/.crabcc/telemetry.jsonl` and return a non-blocking writer.
fn telemetry_file_writer() -> Option<(
    PathBuf,
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
)> {
    let path = std::env::var_os("CRABCC_TELEMETRY_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|d| d.join(".crabcc").join("telemetry.jsonl"))
        })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let (w, g) = tracing_appender::non_blocking(file);
    Some((path, w, g))
}
