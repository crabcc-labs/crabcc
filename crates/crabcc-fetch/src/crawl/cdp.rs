//! Minimal Chrome DevTools Protocol client over a `ws://` connection.
//!
//! CDP is JSON-over-WebSocket *by spec* — this speaks it to Lightpanda
//! exactly as Puppeteer/Playwright do (no binary/msgpack wire; the browser
//! doesn't offer one). It implements just the compact surface the crawler
//! needs, driven sequentially over one connection: browser contexts
//! (isolation), tabs/targets (create, attach with `flatten`, activate for
//! switching, close), navigation (`Page.navigate` + `loadEventFired`), and
//! the rendered DOM (`Runtime.evaluate` of `outerHTML`).
//!
//! Fuller CDP (input, cookies, screenshots, the Network domain for real
//! status codes) is deliberately out of scope — "compressed, fewer tools".
//!
//! NOTE: compile-checked + unit-tested at the frame layer, but the live
//! WebSocket drive against a real Lightpanda is **not** exercised in CI
//! (no browser in the sandbox). Every failure path is surfaced as `Err` so
//! the caller can fall back to the native HTTP transport.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A sequential CDP connection. One outstanding command at a time, which
/// is all the crawler's tab-per-page flow needs.
pub struct Cdp {
    ws: Ws,
    next_id: i64,
}

/// A parsed inbound CDP frame: either a command reply (correlated by `id`)
/// or an unsolicited event (`method`, optionally session-scoped).
#[derive(Debug, PartialEq)]
enum Frame {
    Result {
        id: i64,
        result: Value,
    },
    Error {
        id: i64,
        message: String,
    },
    Event {
        method: String,
        session: Option<String>,
    },
}

impl Cdp {
    /// Open a CDP connection to a `ws://host:port` endpoint.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .with_context(|| format!("CDP connect {ws_url}"))?;
        Ok(Self { ws, next_id: 0 })
    }

    /// Parse one CDP text frame. Pure — the unit-tested core. Returns
    /// `None` for JSON that is neither a reply nor a recognizable event.
    fn parse_frame(text: &str) -> Option<Frame> {
        let v: Value = serde_json::from_str(text).ok()?;
        if let Some(id) = v.get("id").and_then(Value::as_i64) {
            if let Some(err) = v.get("error") {
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("CDP error")
                    .to_string();
                return Some(Frame::Error { id, message });
            }
            let result = v.get("result").cloned().unwrap_or(Value::Null);
            return Some(Frame::Result { id, result });
        }
        let method = v.get("method")?.as_str()?.to_string();
        let session = v.get("sessionId").and_then(Value::as_str).map(String::from);
        Some(Frame::Event { method, session })
    }

    /// Read the next protocol frame, transparently answering pings and
    /// skipping frames we don't model.
    async fn read_frame(&mut self) -> Result<Frame> {
        loop {
            let msg = self
                .ws
                .next()
                .await
                .ok_or_else(|| anyhow!("CDP stream closed"))??;
            match msg {
                Message::Text(t) => {
                    if let Some(f) = Self::parse_frame(t.as_str()) {
                        return Ok(f);
                    }
                }
                Message::Ping(p) => {
                    self.ws.send(Message::Pong(p)).await.ok();
                }
                Message::Close(_) => bail!("CDP connection closed by browser"),
                _ => {} // binary / pong — ignore
            }
        }
    }

    fn encode(&mut self, method: &str, params: Value, session: Option<&str>) -> (i64, Message) {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::from(s);
        }
        (id, Message::Text(msg.to_string().into()))
    }

    /// Send a command and await its reply, ignoring interleaved events.
    pub async fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value> {
        let (id, frame) = self.encode(method, params, session);
        self.ws
            .send(frame)
            .await
            .with_context(|| format!("CDP send {method}"))?;
        loop {
            match self.read_frame().await? {
                Frame::Result { id: rid, result } if rid == id => return Ok(result),
                Frame::Error { id: rid, message } if rid == id => bail!("CDP {method}: {message}"),
                _ => continue,
            }
        }
    }

    /// Create an isolated browser context; returns its id.
    pub async fn create_browser_context(&mut self) -> Result<String> {
        let r = self
            .call("Target.createBrowserContext", json!({}), None)
            .await?;
        string_field(&r, "browserContextId")
    }

    /// Dispose a browser context (best-effort cleanup).
    pub async fn dispose_browser_context(&mut self, context: &str) -> Result<()> {
        self.call(
            "Target.disposeBrowserContext",
            json!({ "browserContextId": context }),
            None,
        )
        .await
        .map(|_| ())
    }

    /// Open a new tab (target) at `url`, optionally inside `context`.
    pub async fn create_target(&mut self, url: &str, context: Option<&str>) -> Result<String> {
        let mut params = json!({ "url": url });
        if let Some(c) = context {
            params["browserContextId"] = Value::from(c);
        }
        let r = self.call("Target.createTarget", params, None).await?;
        string_field(&r, "targetId")
    }

    /// Attach to a target with a flattened session; returns the session id
    /// used to scope subsequent page/runtime commands.
    pub async fn attach(&mut self, target: &str) -> Result<String> {
        let r = self
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
                None,
            )
            .await?;
        string_field(&r, "sessionId")
    }

    /// Bring a target to the foreground (tab switching). Part of the
    /// compact CDP surface; the single-tab-per-fetch crawl flow doesn't
    /// switch tabs, so the engine doesn't currently call it.
    #[allow(dead_code)]
    pub async fn activate_target(&mut self, target: &str) -> Result<()> {
        self.call("Target.activateTarget", json!({ "targetId": target }), None)
            .await
            .map(|_| ())
    }

    /// Close a target (tab).
    pub async fn close_target(&mut self, target: &str) -> Result<()> {
        self.call("Target.closeTarget", json!({ "targetId": target }), None)
            .await
            .map(|_| ())
    }

    /// Navigate the session's page to `url` and wait (best-effort, bounded
    /// by `load_timeout`) for the load event. A timeout is not fatal — the
    /// DOM may already be populated — but a `Page.navigate` error is.
    pub async fn navigate(
        &mut self,
        session: &str,
        url: &str,
        load_timeout: Duration,
    ) -> Result<()> {
        self.call("Page.enable", json!({}), Some(session)).await?;
        let (nav_id, frame) = self.encode("Page.navigate", json!({ "url": url }), Some(session));
        self.ws
            .send(frame)
            .await
            .with_context(|| format!("CDP send Page.navigate {url}"))?;

        // Either the reply or the load event can arrive first; settle once
        // we've seen both (or the timeout fires).
        let mut got_reply = false;
        let mut got_load = false;
        let waited = tokio::time::timeout(load_timeout, async {
            loop {
                match self.read_frame().await? {
                    Frame::Result { id, .. } if id == nav_id => got_reply = true,
                    Frame::Error { id, message } if id == nav_id => {
                        bail!("Page.navigate: {message}")
                    }
                    Frame::Event { method, session: s }
                        if method == "Page.loadEventFired" && s.as_deref() == Some(session) =>
                    {
                        got_load = true;
                    }
                    _ => {}
                }
                if got_reply && got_load {
                    return Ok(());
                }
            }
        })
        .await;
        match waited {
            Ok(inner) => inner,      // Ok(()) or a hard navigate error
            Err(_elapsed) => Ok(()), // best-effort: proceed to read the DOM
        }
    }

    /// Pull the rendered DOM (`document.documentElement.outerHTML`).
    pub async fn outer_html(&mut self, session: &str) -> Result<String> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "document.documentElement.outerHTML",
                    "returnByValue": true
                }),
                Some(session),
            )
            .await?;
        r.get("result")
            .and_then(|res| res.get("value"))
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow!("Runtime.evaluate returned no string value"))
    }

    /// The page's current URL (`window.location.href`) — used to resolve
    /// links against the post-redirect address.
    pub async fn current_url(&mut self, session: &str) -> Result<String> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": "window.location.href", "returnByValue": true }),
                Some(session),
            )
            .await?;
        r.get("result")
            .and_then(|res| res.get("value"))
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow!("Runtime.evaluate returned no location.href"))
    }
}

fn string_field(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow!("CDP reply missing `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_command_result() {
        let f = Cdp::parse_frame(r#"{"id":7,"result":{"targetId":"T1"}}"#).unwrap();
        assert_eq!(
            f,
            Frame::Result {
                id: 7,
                result: json!({ "targetId": "T1" })
            }
        );
    }

    #[test]
    fn parses_command_error() {
        let f = Cdp::parse_frame(r#"{"id":3,"error":{"code":-32000,"message":"boom"}}"#).unwrap();
        assert_eq!(
            f,
            Frame::Error {
                id: 3,
                message: "boom".into()
            }
        );
    }

    #[test]
    fn parses_session_scoped_event() {
        let f =
            Cdp::parse_frame(r#"{"method":"Page.loadEventFired","sessionId":"S9","params":{}}"#)
                .unwrap();
        assert_eq!(
            f,
            Frame::Event {
                method: "Page.loadEventFired".into(),
                session: Some("S9".into())
            }
        );
    }

    #[test]
    fn parses_unscoped_event() {
        let f = Cdp::parse_frame(r#"{"method":"Target.targetCreated","params":{}}"#).unwrap();
        assert_eq!(
            f,
            Frame::Event {
                method: "Target.targetCreated".into(),
                session: None
            }
        );
    }

    #[test]
    fn rejects_non_frame_json() {
        assert!(Cdp::parse_frame(r#"{"hello":"world"}"#).is_none());
        assert!(Cdp::parse_frame("not json").is_none());
    }

    #[test]
    fn string_field_extracts_or_errors() {
        assert_eq!(string_field(&json!({"a":"b"}), "a").unwrap(), "b");
        assert!(string_field(&json!({"a":1}), "a").is_err());
        assert!(string_field(&json!({}), "missing").is_err());
    }
}
