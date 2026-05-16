//! Background workers that feed [`super::AppState`] via the shared
//! `flume` channel. Separated from the state struct itself so the
//! threading machinery is easy to read in isolation.

use tracing::{debug, info};

use super::{AppEvent, Prefetch};
use crate::api::Client;

/// Bounded buffer for the multiplexed `AppEvent` channel. Four
/// background workers (prefetch + SSE bridge + telemetry poll +
/// memory poll) plus the four UI submit paths funnel through this
/// channel; the gpui pump drains it on the main thread. Cap of 512
/// is ~3 minutes of runway at the union of typical worker rates;
/// overflow (a stuck pump) logs a warn-level line and drops the
/// individual event rather than block any worker. See the
/// `try_send_app_event` helper for the policy.
const APP_CHANNEL_CAP: usize = 512;

/// Telemetry poll interval. 3s matches the existing web `usePolling.ts`
/// cadence on the React side; tuned to surface logs near-real-time
/// without hammering the server when nothing's happening.
const TELEMETRY_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// Memory drawer refresh cadence. Drawers churn slower than telemetry
/// — the typical write rate is "human ingest from CLI", so 10s is fine.
const MEMORY_POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// Dispatch a [`crate::routes::commands::RunnableCommand`] to the
/// matching [`Client`] method. Used by the worker thread spawned in
/// [`super::AppState::submit_command_run`]. Returns the response as a
/// pretty-printed JSON string — every response type derives
/// `Serialize` (sweep landed in this PR), so the launchpad's inline
/// result block reads as proper JSON instead of Rust Debug format.
pub(super) fn run_command(
    client: &Client,
    cmd: crate::routes::commands::RunnableCommand,
    tx: &flume::Sender<AppEvent>,
) -> anyhow::Result<String> {
    use crate::routes::commands::RunnableCommand as RC;
    fn pretty<T: serde::Serialize>(v: T) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&v)?)
    }
    match cmd {
        RC::Health => pretty(client.health()?),
        RC::Bootstrap => pretty(client.bootstrap()?),
        RC::Services => pretty(client.services()?),
        RC::Agents => pretty(client.agents()?),
        RC::AgentProfiles => pretty(client.agent_profiles()?),
        RC::AgentKills => pretty(client.agent_kills()?),
        RC::AgentModels => pretty(client.agent_models()?),
        RC::OllamaKey => pretty(client.ollama_key()?),
        RC::OtlpHealth => pretty(client.otlp_health()?),
        RC::Reindex => pretty(client.reindex()?),
        RC::RandomQuery => pretty(client.random_query()?),
        RC::SeedGraph => pretty(client.seed_graph()?),
        RC::MemoryRecent => pretty(client.memory_recent()?),
        RC::TestSampling => run_test_sampling(tx),
    }
}

/// Smoke test for the MCP sampling-offer. Builds a
/// [`crate::sampling::LiteLlmSamplingHandler`] with the inspector observer
/// attached and fires a small `sampling/createMessage` so the route layer
/// can show a real round-trip end-to-end. Returns the response JSON for
/// inline display.
fn run_test_sampling(tx: &flume::Sender<AppEvent>) -> anyhow::Result<String> {
    use crate::sampling::{
        Content, LiteLlmSamplingHandler, Message, ModelHint, ModelPreferences, Role,
        SamplingHandler, SamplingMeta, SamplingRequest,
    };
    use std::sync::Arc;

    let handler = LiteLlmSamplingHandler::from_env()
        .map_err(|e| anyhow::anyhow!("build handler: {e}"))?
        .with_observer(Arc::new(crate::inspector::InspectorSamplingObserver::new(
            tx.clone(),
        )));

    // Tight smoke prompt — small max_tokens so this returns in a
    // few seconds even on cold-loaded qwen3.5:35b.
    let request = SamplingRequest {
        messages: vec![Message {
            role: Role::User,
            content: Content::Text {
                text: "Say 'inspector loop is live' in three words exactly.".into(),
            },
        }],
        model_preferences: Some(ModelPreferences {
            hints: vec![
                ModelHint {
                    name: "qwen3.5".into(),
                },
                ModelHint {
                    name: "qwen2.5-coder".into(),
                },
            ],
            cost_priority: Some(1.0),
            ..Default::default()
        }),
        system_prompt: Some("You are terse.".into()),
        max_tokens: Some(64),
        stop_sequences: vec!["</think>".into()],
        temperature: Some(0.2),
        include_context: None,
        meta: Some(SamplingMeta { sampling_depth: 0 }),
    };

    let response = handler
        .handle(request)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(serde_json::to_string_pretty(&response)?)
}

/// Best-effort `AppEvent` send. Drops the event (with a warn log)
/// if the channel is full — preferable to blocking a worker thread
/// on a stuck pump. Returns `Ok(())` on successful send, `Err(())`
/// when the receiver has been dropped (caller should treat as
/// shutdown signal).
pub(super) fn try_send_app_event(tx: &flume::Sender<AppEvent>, evt: AppEvent) -> Result<(), ()> {
    match tx.try_send(evt) {
        Ok(()) => Ok(()),
        Err(flume::TrySendError::Disconnected(_)) => Err(()),
        Err(flume::TrySendError::Full(_)) => {
            tracing::warn!(
                target: "crabcc::state",
                cap = APP_CHANNEL_CAP,
                "app-event channel full, dropping event"
            );
            // We deliberately don't propagate as Err here — the
            // channel is still alive, just saturated. Caller treats
            // the same as a successful send for control-flow purposes.
            Ok(())
        }
    }
}

/// Spawn the four background workers and return both the receiver
/// (drained by the gpui pump via [`super::AppState::pump_events`]) and a
/// cloned sender — the latter is stashed on `AppState::ingest_tx` so
/// one-shot UI-driven work (memory ingest, future agent spawn) can
/// post events back into the same channel from a detached thread.
///
/// Workers run on their own OS threads — no async runtime needed, and
/// [`flume::Receiver::recv_async`] works inside gpui's smol-flavored
/// `cx.spawn`.
pub fn spawn_workers(base_url: &str) -> (flume::Sender<AppEvent>, flume::Receiver<AppEvent>) {
    let (tx, rx) = flume::bounded::<AppEvent>(APP_CHANNEL_CAP);

    // One-shot prefetch — bootstrap + services + seed-graph all on the
    // same thread. The seed-graph response is ~20 KB / 96 nodes today
    // so an extra HTTP round-trip at startup is fine; promote to a
    // background-on-demand fetch if the graph grows large.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-prefetch".into())
            .spawn(move || {
                debug!(target: "crabcc::state", thread = "prefetch", "starting");
                let client = Client::with_base_url(base);
                let prefetch = Prefetch {
                    bootstrap: client.bootstrap(),
                    services: client.services(),
                    graph: client.seed_graph(),
                    memory_recent: client.memory_recent(),
                    otlp_health: client.otlp_health(),
                    agent_profiles: client.agent_profiles(),
                    agent_kills: client.agent_kills(),
                    agent_models: client.agent_models(),
                    ollama_key: client.ollama_key(),
                };
                // Receiver disconnect is fine — app shutdown raced us.
                let _ = try_send_app_event(&tx, AppEvent::Initial(Box::new(prefetch)));
                debug!(target: "crabcc::state", thread = "prefetch", "exiting");
            })
            .expect("prefetch thread spawn");
    }

    // Long-lived SSE pump. Wrap each `SseEvent` in `AppEvent::Sse` on
    // its way out so `AppState::apply` only has one match arm shape.
    let sse_rx = crate::sse::spawn_worker(base_url);
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("crabcc-sse-bridge".into())
            .spawn(move || {
                info!(target: "crabcc::state", thread = "sse-bridge", "starting");
                while let Ok(evt) = sse_rx.recv() {
                    if try_send_app_event(&tx, AppEvent::Sse(evt)).is_err() {
                        info!(target: "crabcc::state", thread = "sse-bridge", "exiting (rx dropped)");
                        return;
                    }
                }
                info!(target: "crabcc::state", thread = "sse-bridge", "exiting (sse channel closed)");
            })
            .expect("sse bridge thread spawn");
    }

    // Long-lived telemetry poller. Synchronous loop on its own thread,
    // sleeps `TELEMETRY_POLL` between attempts. We don't track the
    // cursor inside the worker — the gpui-side `AppState::apply` owns
    // it, but the worker passes the latest known cursor back through
    // its own captured copy so we don't reset on transient failures.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-telemetry".into())
            .spawn(move || {
                info!(target: "crabcc::state", thread = "telemetry", "starting");
                let client = Client::with_base_url(base);
                let mut cursor: i64 = 0;
                loop {
                    if tx.is_disconnected() {
                        info!(target: "crabcc::state", thread = "telemetry", "exiting (rx dropped)");
                        return;
                    }
                    let result = client.telemetry(Some(cursor), 100);
                    if let Ok(snapshot) = &result {
                        cursor = snapshot.cursor as i64;
                    }
                    if try_send_app_event(&tx, AppEvent::Telemetry(result)).is_err() {
                        info!(target: "crabcc::state", thread = "telemetry", "exiting (send fail)");
                        return;
                    }
                    std::thread::sleep(TELEMETRY_POLL);
                }
            })
            .expect("telemetry thread spawn");
    }

    // Long-lived memory-drawer poller. Slower cadence than telemetry —
    // drawer creation is a human-driven event from `crabcc memory
    // ingest`, so 10s is plenty. Mirrors the telemetry pattern: GET,
    // send, sleep, repeat.
    {
        let tx = tx.clone();
        let base = base_url.to_string();
        std::thread::Builder::new()
            .name("crabcc-memory-poll".into())
            .spawn(move || {
                info!(target: "crabcc::state", thread = "memory-poll", "starting");
                let client = Client::with_base_url(base);
                loop {
                    if tx.is_disconnected() {
                        info!(target: "crabcc::state", thread = "memory-poll", "exiting (rx dropped)");
                        return;
                    }
                    // First tick fires immediately after `MEMORY_POLL`
                    // — the prefetch worker already covered the cold
                    // path, so we can sleep before the first GET to
                    // skip a redundant fetch at startup.
                    std::thread::sleep(MEMORY_POLL);
                    if tx
                        .send(AppEvent::MemoryRefresh(client.memory_recent()))
                        .is_err()
                    {
                        info!(target: "crabcc::state", thread = "memory-poll", "exiting (send fail)");
                        return;
                    }
                }
            })
            .expect("memory-poll thread spawn");
    }

    (tx, rx)
}
