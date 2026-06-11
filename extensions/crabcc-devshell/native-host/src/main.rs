use anyhow::Result;
use serde_json::Value;
use std::io::{Read, Write};

fn read_msg(r: &mut impl Read) -> Result<Option<Value>> {
    let mut len_buf = [0u8; 4];
    if r.read_exact(&mut len_buf).is_err() {
        return Ok(None); // EOF
    }
    let len = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

fn write_msg(w: &mut impl Write, val: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(val)?;
    let len = bytes.len() as u32;
    w.write_all(&len.to_ne_bytes())?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    loop {
        let msg = match read_msg(&mut stdin)? {
            Some(m) => m,
            None => break,
        };

        // Print the console event to stderr (visible in devshell terminal)
        let level = msg.get("level").and_then(|v| v.as_str()).unwrap_or("log");
        let args = msg.get("args").and_then(|v| v.as_array()).map(|a| {
            a.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ")
        }).unwrap_or_default();
        let url = msg.get("url").and_then(|v| v.as_str()).unwrap_or("?");
        eprintln!("[browser:{level}] {args}  <{url}>");

        // Echo ack back to extension
        write_msg(&mut stdout, &serde_json::json!({ "ack": true }))?;
    }
    Ok(())
}
