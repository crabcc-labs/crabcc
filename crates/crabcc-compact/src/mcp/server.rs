use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use super::tools;

pub fn run(port: u16) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)?;
    println!("crabcc-compact MCP server on http://{addr}");
    println!("Wire: claude mcp add --transport sse compact http://{addr}/sse");

    let sessions: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let sessions = Arc::clone(&sessions);
        thread::spawn(move || {
            let _ = handle_connection(stream, sessions);
        });
    }
    Ok(())
}

fn handle_connection(
    mut stream: std::net::TcpStream,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.to_ascii_lowercase().starts_with("content-length:") {
            content_length = trimmed
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
        }
    }

    let rl = request_line.trim().to_string();
    if rl.contains("GET /sse") || rl.contains("GET /sse?") {
        handle_sse(stream, sessions)?;
    } else if rl.starts_with("POST /message") {
        let session_id = extract_session_id(&rl);
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
        let body = String::from_utf8_lossy(&body).to_string();
        handle_message(&mut stream, &session_id, &body, sessions)?;
    } else {
        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")?;
    }
    Ok(())
}

fn handle_sse(
    mut stream: std::net::TcpStream,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
) -> anyhow::Result<()> {
    let session_id = format!(
        "s{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let (tx, rx) = mpsc::channel::<String>();
    sessions.lock().unwrap().insert(session_id.clone(), tx);

    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
    )?;
    stream.write_all(
        format!("event: endpoint\r\ndata: /message?sessionId={session_id}\r\n\r\n").as_bytes(),
    )?;

    for msg in rx {
        if stream
            .write_all(format!("event: message\r\ndata: {msg}\r\n\r\n").as_bytes())
            .is_err()
        {
            break;
        }
    }
    sessions.lock().unwrap().remove(&session_id);
    Ok(())
}

fn handle_message(
    stream: &mut std::net::TcpStream,
    session_id: &str,
    body: &str,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_str(body).unwrap_or(json!(null));
    let id = req.get("id").cloned().unwrap_or(json!(null));
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "crabcc-compact", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
        "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": tools::list_tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let tool_params = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = tools::call_tool(name, &tool_params);
            json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "content": [{"type": "text", "text": serde_json::to_string(&result.content)?}],
                    "isError": result.is_error
                }
            })
        }
        _ => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}}),
    };

    let resp_str = serde_json::to_string(&response)?;
    let sent = {
        let guard = sessions.lock().unwrap();
        guard
            .get(session_id)
            .map(|tx| tx.send(resp_str.clone()).is_ok())
            .unwrap_or(false)
    };

    if !sent {
        let r = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resp_str}", resp_str.len());
        stream.write_all(r.as_bytes())?;
    } else {
        stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")?;
    }
    Ok(())
}

fn extract_session_id(request_line: &str) -> String {
    request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .split('?')
        .nth(1)
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("sessionId=").map(|s| s.to_string()))
        .unwrap_or_default()
}
