//! Blocking SSE worker — connects to `/api/events`, parses the
//! `event:`/`data:` frames, and sends typed events through a `flume`
//! channel that the gpui side drains via `recv_async()`.
//!
//! Why a `std::thread` + blocking reqwest instead of async streaming:
//! gpui's executor is smol-flavored and reqwest 0.12's async client
//! pulls a tokio runtime. Bridging the two adds a thread + channel
//! either way; doing the work in a plain `std::thread` with
//! `Response: Read` is one fewer moving part. The `flume` async
//! receiver is runtime-agnostic, so the gpui-side consumer doesn't
//! care that the producer is blocking.
//!
//! Reconnect: on stream close or transport error, sleep with exponential
//! backoff (1s → 30s) and retry. Drops out of the loop only when the
//! channel's receiving end is gone.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::api::types::{SseActivityFrame, SseAgentsFrame};

#[derive(Debug, Clone)]
pub enum SseEvent {
    Activity(SseActivityFrame),
    Agents(SseAgentsFrame),
    /// A topic the desktop client doesn't know about yet. Surface the
    /// raw payload so future code can introspect without reshipping
    /// the worker.
    Unknown {
        topic: String,
        data: serde_json::Value,
    },
}

/// Spawn the SSE worker on its own OS thread. Returns the receiving
/// end of the event channel — drop the receiver to stop the worker.
pub fn spawn_worker(base_url: impl Into<String>) -> flume::Receiver<SseEvent> {
    let url = format!("{}/api/events", base_url.into());
    let (tx, rx) = flume::unbounded::<SseEvent>();
    std::thread::Builder::new()
        .name("crabcc-sse".into())
        .spawn(move || run(&url, &tx))
        .expect("OS lets us spawn one thread");
    rx
}

fn run(url: &str, tx: &flume::Sender<SseEvent>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if tx.is_disconnected() {
            return;
        }
        match connect_and_pump(url, tx) {
            Ok(()) => {
                // Server closed cleanly — short delay before reconnect,
                // reset the backoff window so a flaky server doesn't
                // permanently inflate it.
                eprintln!("crabcc-sse: stream ended, reconnecting in 1s");
                std::thread::sleep(Duration::from_secs(1));
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                eprintln!("crabcc-sse: {e:?}; backing off {backoff:?}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn connect_and_pump(url: &str, tx: &flume::Sender<SseEvent>) -> Result<()> {
    // Dedicated client for SSE — no overall request timeout so a quiet
    // server doesn't trip the default. The other API calls in
    // `api::client` keep their own 5-second timeout.
    let http = reqwest::blocking::Client::builder()
        .timeout(None::<Duration>)
        .build()
        .context("build SSE http client")?;
    let resp = http.get(url).send().with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("{url} → {}", resp.status());
    }
    let reader = BufReader::new(resp);
    let mut event_topic: Option<String> = None;
    let mut data_buf = String::new();
    for line in reader.lines() {
        let line = line.context("SSE line read")?;
        if line.is_empty() {
            // Blank line == end of frame. Dispatch what we have.
            if let Some(topic) = event_topic.take() {
                if !data_buf.is_empty() {
                    if let Some(evt) = parse_frame(&topic, &data_buf) {
                        if tx.send(evt).is_err() {
                            return Ok(()); // receiver gone
                        }
                    }
                }
            }
            data_buf.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event_topic = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            // Per the SSE spec, multi-line `data:` fields concatenate
            // with a literal newline. The crabcc server emits single-
            // line frames today; this preserves the standard regardless.
            if !data_buf.is_empty() {
                data_buf.push('\n');
            }
            data_buf.push_str(rest.trim_start());
        }
        // `:`-prefixed comments and `id:` / `retry:` fields are ignored.
    }
    Ok(())
}

fn parse_frame(topic: &str, data: &str) -> Option<SseEvent> {
    match topic {
        "activity" => match serde_json::from_str::<SseActivityFrame>(data) {
            Ok(f) => Some(SseEvent::Activity(f)),
            Err(e) => {
                eprintln!("crabcc-sse: activity decode failed: {e}");
                None
            }
        },
        "agents" => match serde_json::from_str::<SseAgentsFrame>(data) {
            Ok(f) => Some(SseEvent::Agents(f)),
            Err(e) => {
                eprintln!("crabcc-sse: agents decode failed: {e}");
                None
            }
        },
        other => {
            let value: serde_json::Value = serde_json::from_str(data)
                .unwrap_or_else(|_| serde_json::Value::String(data.to_string()));
            Some(SseEvent::Unknown {
                topic: other.to_string(),
                data: value,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::AgentStatus;

    #[test]
    fn parse_activity_frame_from_live_sample() {
        let data = r#"{"repo":"crabcc","cursor":1777664931,"events":[{"ts":1777576223,"op":"sym","query":"Store","results":1}]}"#;
        let evt = parse_frame("activity", data).expect("decoded");
        match evt {
            SseEvent::Activity(f) => {
                assert_eq!(f.repo, "crabcc");
                assert_eq!(f.events.len(), 1);
                assert_eq!(f.events[0].op, "sym");
                assert_eq!(f.events[0].results, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_agents_frame_from_live_sample() {
        let data = r#"{"agents":[{"id":"abc","status":"running","started_ts":0,"pid":null,"runtime":"subprocess (host)","model":null,"prompt_preview":"","log_bytes":0,"root":null}]}"#;
        let evt = parse_frame("agents", data).expect("decoded");
        match evt {
            SseEvent::Agents(f) => {
                assert_eq!(f.agents.len(), 1);
                assert_eq!(f.agents[0].id, "abc");
                assert_eq!(f.agents[0].status, AgentStatus::Running);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_unknown_topic_preserved_verbatim() {
        let evt = parse_frame("future-topic", r#"{"foo":42}"#).expect("decoded");
        match evt {
            SseEvent::Unknown { topic, data } => {
                assert_eq!(topic, "future-topic");
                assert_eq!(data["foo"], 42);
            }
            _ => panic!("wrong variant"),
        }
    }
}
