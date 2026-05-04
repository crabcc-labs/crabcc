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
use tracing::{debug, error, info, warn};

use crate::api::types::{SseActivityFrame, SseAgentsFrame};

#[derive(Debug, Clone)]
pub enum SseEvent {
    Activity(SseActivityFrame),
    Agents(SseAgentsFrame),
    /// A topic the desktop client doesn't know about yet. Surface the
    /// raw payload so future code can introspect without reshipping
    /// the worker.
    Unknown {
        topic: Box<str>,
        data: serde_json::Value,
    },
}

/// Parsed topic — avoids allocating strings for the two known topics.
enum ParsedTopic {
    Activity,
    Agents,
    Unknown(String),
}

/// Bounded buffer for SSE events headed to the gpui pump. ~85 s of
/// runway at the typical 3-events/s cadence — long enough that a
/// transient gpui-side stall (a slow render, a debugger pause)
/// doesn't drop frames, short enough that a stuck consumer can't
/// monotonically grow memory. Overflow drops the *newest* event with
/// a warn-level log line; see `try_send_with_overflow`. Drop-oldest
/// would be more elegant but requires the worker to clone a drain
/// receiver, which complicates disconnect detection.
const SSE_CHANNEL_CAP: usize = 256;

/// Spawn the SSE worker on its own OS thread. Returns the receiving
/// end of the event channel — drop the receiver to stop the worker.
pub fn spawn_worker(base_url: impl AsRef<str>) -> flume::Receiver<SseEvent> {
    let url = format!("{}/api/events", base_url.as_ref());
    info!(target: "crabcc::sse", %url, cap = SSE_CHANNEL_CAP, "spawning SSE worker");
    let (tx, rx) = flume::bounded::<SseEvent>(SSE_CHANNEL_CAP);
    std::thread::Builder::new()
        .name("crabcc-sse".into())
        .spawn(move || run(&url, &tx))
        .expect("OS lets us spawn one thread");
    rx
}

fn run(url: &str, tx: &flume::Sender<SseEvent>) {
    // Build the HTTP client ONCE and reuse it across reconnects. Each
    // `Client` carries a connection pool + a TLS session cache; rebuilding
    // it on every reconnect (the original shape) threw away those caches
    // and forced a fresh handshake even for a brief reconnect blip. The
    // `timeout(None)` is critical for SSE — a quiet server is normal, not
    // a fault. Other `api::client` callers still use their own 5-second
    // bounded client.
    let http = match reqwest::blocking::Client::builder()
        .timeout(None::<Duration>)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!(
                target: "crabcc::sse",
                error = %e,
                "failed to build http client; worker exiting"
            );
            return;
        }
    };

    let mut backoff = Duration::from_secs(1);
    loop {
        if tx.is_disconnected() {
            info!(target: "crabcc::sse", "receiver dropped; worker exiting");
            return;
        }
        match connect_and_pump(&http, url, tx) {
            Ok(()) => {
                // Server closed cleanly — short delay before reconnect,
                // reset the backoff window so a flaky server doesn't
                // permanently inflate it.
                info!(target: "crabcc::sse", "stream ended cleanly, reconnecting in 1s");
                std::thread::sleep(Duration::from_secs(1));
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                warn!(
                    target: "crabcc::sse",
                    error = ?e,
                    backoff_ms = backoff.as_millis() as u64,
                    "stream errored; backing off"
                );
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn connect_and_pump(
    http: &reqwest::blocking::Client,
    url: &str,
    tx: &flume::Sender<SseEvent>,
) -> Result<()> {
    debug!(target: "crabcc::sse", %url, "opening SSE connection");
    let resp = http.get(url).send().with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("{url} → {}", resp.status());
    }
    let mut reader = BufReader::new(resp);

    // Reusable byte buffers — avoids per-line String allocations and
    // redundant UTF-8 validation (serde_json validates during parse).
    let mut line_buf = Vec::new();
    let mut data_buf = Vec::new();
    let mut current_topic: Option<ParsedTopic> = None;

    loop {
        line_buf.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line_buf)
            .context("SSE line read")?;
        if bytes_read == 0 {
            break; // EOF
        }

        // Strip trailing CR/LF.
        let line = strip_line_ending(&line_buf);

        if line.is_empty() {
            // Blank line == end of frame. Dispatch what we have.
            if let Some(topic) = current_topic.take() {
                if !data_buf.is_empty() {
                    if let Some(evt) = parse_frame(topic, &data_buf) {
                        match tx.try_send(evt) {
                            Ok(()) => {}
                            Err(flume::TrySendError::Disconnected(_)) => {
                                // Receiver dropped — exit cleanly.
                                return Ok(());
                            }
                            Err(flume::TrySendError::Full(_)) => {
                                // Bounded channel saturated. Drop the
                                // newest frame (the one we just tried
                                // to send) rather than block the SSE
                                // reader — the gpui pump will catch up
                                // on the next ungated tick. Logged at
                                // warn level so a chronically stalled
                                // consumer is visible.
                                warn!(
                                    target: "crabcc::sse",
                                    cap = SSE_CHANNEL_CAP,
                                    "channel full, dropping SSE frame"
                                );
                            }
                        }
                    }
                }
            }
            data_buf.clear();
            continue;
        }

        if let Some(rest) = line.strip_prefix(b"event:") {
            let topic_str = trim_ascii(rest);
            current_topic = Some(match topic_str {
                b"activity" => ParsedTopic::Activity,
                b"agents" => ParsedTopic::Agents,
                other => ParsedTopic::Unknown(String::from_utf8_lossy(other).into_owned()),
            });
        } else if let Some(rest) = line.strip_prefix(b"data:") {
            // Per the SSE spec, multi-line `data:` fields concatenate
            // with a literal newline. The crabcc server emits single-
            // line frames today; this preserves the standard regardless.
            if !data_buf.is_empty() {
                data_buf.push(b'\n');
            }
            data_buf.extend_from_slice(trim_ascii_start(rest));
        }
        // `:`-prefixed comments and `id:` / `retry:` fields are ignored.
    }
    Ok(())
}

/// Strip trailing `\n` and `\r` from a byte slice.
fn strip_line_ending(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }
    &buf[..end]
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ascii(buf: &[u8]) -> &[u8] {
    let start = buf
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .unwrap_or(buf.len());
    let end = buf
        .iter()
        .rposition(|&b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    &buf[start..end]
}

/// Trim leading ASCII whitespace from a byte slice.
fn trim_ascii_start(buf: &[u8]) -> &[u8] {
    let start = buf
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .unwrap_or(buf.len());
    &buf[start..]
}

fn parse_frame(topic: ParsedTopic, data: &[u8]) -> Option<SseEvent> {
    match topic {
        ParsedTopic::Activity => match serde_json::from_slice::<SseActivityFrame>(data) {
            Ok(f) => Some(SseEvent::Activity(f)),
            Err(e) => {
                warn!(target: "crabcc::sse", error = %e, topic = "activity", "frame decode failed");
                None
            }
        },
        ParsedTopic::Agents => match serde_json::from_slice::<SseAgentsFrame>(data) {
            Ok(f) => Some(SseEvent::Agents(f)),
            Err(e) => {
                warn!(target: "crabcc::sse", error = %e, topic = "agents", "frame decode failed");
                None
            }
        },
        ParsedTopic::Unknown(other) => {
            let value: serde_json::Value = serde_json::from_slice(data).unwrap_or_else(|_| {
                let s = match std::str::from_utf8(data) {
                    Ok(valid) => valid.to_owned(),
                    Err(_) => String::from_utf8_lossy(data).into_owned(),
                };
                serde_json::Value::String(s)
            });
            Some(SseEvent::Unknown {
                topic: other.into_boxed_str(),
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
        let data = br#"{"repo":"crabcc","cursor":1777664931,"events":[{"ts":1777576223,"op":"sym","query":"Store","results":1}]}"#;
        let evt = parse_frame(ParsedTopic::Activity, data).expect("decoded");
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
        let data = br#"{"agents":[{"id":"abc","status":"running","started_ts":0,"pid":null,"runtime":"subprocess (host)","model":null,"prompt_preview":"","log_bytes":0,"root":null}]}"#;
        let evt = parse_frame(ParsedTopic::Agents, data).expect("decoded");
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
        let evt = parse_frame(
            ParsedTopic::Unknown("future-topic".to_string()),
            br#"{"foo":42}"#,
        )
        .expect("decoded");
        match evt {
            SseEvent::Unknown { topic, data } => {
                assert_eq!(&*topic, "future-topic");
                assert_eq!(data["foo"], 42);
            }
            _ => panic!("wrong variant"),
        }
    }
}
