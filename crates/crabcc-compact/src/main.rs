mod client;
mod config;
mod context;
mod economy;
mod enrich;
mod fallback;
mod gate;
mod mcp;

use anyhow::Result;
use economy::{Budget, pick_ratio};
use serde_json::Value;
use std::io::{self, Read, Write};

fn main() {
    std::panic::set_hook(Box::new(|_| {}));

    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(String::as_str).unwrap_or("");

    let result = match subcommand {
        "posttooluse"  => run_posttooluse(),
        "promptsubmit" => run_promptsubmit(),
        "status"       => cmd_status(),
        "test"         => cmd_test(),
        "economy"      => cmd_economy(),
        "setup"        => cmd_setup(&args[2..]),
        "uninstall"    => cmd_uninstall(&args[2..]),
        "--mcp"        => run_mcp(&args[2..]),
        _ => {
            eprintln!("usage: crabcc-compact <posttooluse|promptsubmit|status|test|economy|setup|uninstall|--mcp>");
            std::process::exit(1);
        }
    };

    if let Err(e) = result {
        log_error(&format!("{e:#}"));
        std::process::exit(0);
    }
}

fn run_posttooluse() -> Result<()> {
    let cfg = config::load()?;
    let mut stdin = String::new();
    io::stdin().read_to_string(&mut stdin)?;

    let json: Value = match serde_json::from_str(&stdin) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let content = extract_tool_result_content(&json);
    let content = match content {
        Some(c) if !c.is_empty() => c,
        _ => return Ok(()),
    };

    if !gate::above_threshold(&content, cfg.threshold_tokens) {
        return Ok(());
    }

    let budget = Budget::new();
    let ratio = pick_ratio(&budget);

    let compressed = match client::compact(&cfg.endpoint, &content, ratio, cfg.timeout_ms) {
        Ok(r) => r.compressed,
        Err(e) => {
            log_error(&format!("compact failed: {e:#}"));
            fallback::truncate(&content, 100, 100)
        }
    };

    let output = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": compressed
        }
    });
    io::stdout().write_all(serde_json::to_string(&output)?.as_bytes())?;
    Ok(())
}

fn extract_tool_result_content(json: &Value) -> Option<String> {
    let tr = json.get("tool_result")?;
    if let Some(s) = tr.as_str() {
        return Some(s.to_string());
    }
    if let Some(content) = tr.get("content") {
        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }
        if let Some(arr) = content.as_array() {
            let text: String = arr.iter()
                .filter_map(|b| b.get("text")?.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() { return Some(text); }
        }
    }
    None
}

pub fn log_error(msg: &str) {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(home).join(".crabcc").join("compact.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}: {msg}", epoch_secs());
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Stubs — implemented in Tasks 11, 12, 13
fn run_promptsubmit() -> Result<()> { Ok(()) }
fn cmd_status() -> Result<()> { println!("status: not yet implemented"); Ok(()) }
fn cmd_test() -> Result<()> { println!("test: not yet implemented"); Ok(()) }
fn cmd_economy() -> Result<()> { println!("economy: not yet implemented"); Ok(()) }
fn cmd_setup(_args: &[String]) -> Result<()> { println!("setup: not yet implemented"); Ok(()) }
fn cmd_uninstall(_args: &[String]) -> Result<()> { println!("uninstall: not yet implemented"); Ok(()) }
fn run_mcp(_args: &[String]) -> Result<()> { println!("mcp: not yet implemented"); Ok(()) }
