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

fn run_promptsubmit() -> Result<()> {
    let cfg = config::load()?;
    let mut stdin = String::new();
    io::stdin().read_to_string(&mut stdin)?;

    let json: Value = match serde_json::from_str(&stdin) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let prompt = match json.get("prompt").and_then(|p| p.as_str()) {
        Some(p) => p.to_string(),
        None => return Ok(()),
    };

    let (prompt_text, do_enrich) = match enrich::detect_trigger(&prompt, &cfg.enrich_trigger) {
        Some(stripped) => (stripped, true),
        None => (prompt.clone(), false),
    };

    if !gate::above_threshold(&prompt_text, cfg.threshold_tokens) && !do_enrich {
        return Ok(());
    }

    let mut budget = Budget::new();
    let ratio = pick_ratio(&budget);

    let compressed = if gate::above_threshold(&prompt_text, cfg.threshold_tokens) {
        match client::compact(&cfg.endpoint, &prompt_text, ratio, cfg.timeout_ms) {
            Ok(r) => {
                budget.record_compress(
                    gate::token_estimate(&prompt_text),
                    gate::token_estimate(&r.compressed),
                );
                r.compressed
            }
            Err(e) => {
                log_error(&format!("promptsubmit compact failed: {e:#}"));
                fallback::truncate(&prompt_text, 50, 50)
            }
        }
    } else {
        prompt_text.clone()
    };

    let final_prompt = if do_enrich {
        match client::enrich(&cfg.endpoint, &compressed, &prompt, cfg.timeout_ms) {
            Ok(r) => format!("{}

---
{compressed}", r.plan),
            Err(e) => {
                log_error(&format!("enrich failed: {e:#}"));
                compressed
            }
        }
    } else {
        compressed
    };

    if final_prompt == prompt {
        return Ok(());
    }

    let output = serde_json::json!({ "updatedPrompt": final_prompt });
    io::stdout().write_all(serde_json::to_string(&output)?.as_bytes())?;
    Ok(())
}

fn cmd_status() -> Result<()> {
    let cfg = config::load()?;
    if cfg.endpoint.is_empty() {
        println!("endpoint: not configured (set endpoint in ~/.config/crabcc/compact.toml)");
        return Ok(());
    }
    match client::health(&cfg.endpoint, cfg.timeout_ms) {
        Ok(v) => println!("ok — {v}"),
        Err(e) => println!("unreachable: {e}"),
    }
    Ok(())
}

fn cmd_test() -> Result<()> {
    let cfg = config::load()?;
    let payload = generate_test_payload();
    println!("sending {}-char payload ({} est. tokens) to {}",
        payload.len(), gate::token_estimate(&payload), cfg.endpoint);
    let start = std::time::Instant::now();
    match client::compact(&cfg.endpoint, &payload, 0.5, cfg.timeout_ms) {
        Ok(r) => {
            let elapsed = start.elapsed();
            let savings_pct = if r.original_tokens > 0 {
                100.0 - (r.compressed_tokens as f32 / r.original_tokens as f32 * 100.0)
            } else { 0.0 };
            println!("ok — {orig} → {comp} tokens ({savings_pct:.1}% saved) in {ms}ms",
                orig = r.original_tokens, comp = r.compressed_tokens, ms = elapsed.as_millis());
        }
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}

fn cmd_economy() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(home).join(".crabcc").join("compact.log");
    if path.exists() {
        let log = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = log.lines().rev().take(20).collect();
        println!("last 20 log entries (most recent first):");
        for l in lines { println!("  {l}"); }
    } else {
        println!("no log yet at {}", path.display());
    }
    Ok(())
}

fn generate_test_payload() -> String {
    let snippet = r#"pub fn handle_request(req: HttpRequest, state: web::Data<AppState>) -> impl Responder {
    let token = req.headers().get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if token.is_empty() { return HttpResponse::Unauthorized().finish(); }
    match state.db.get_user_by_token(token) {
        Ok(user) => HttpResponse::Ok().json(user),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}
"#;
    snippet.repeat(80)
}

fn cmd_setup(args: &[String]) -> Result<()> {
    let host = args.iter()
        .find_map(|a| a.strip_prefix("--host="))
        .unwrap_or("claude-code");
    match host {
        "claude-code" => setup_claude_code()?,
        _ => anyhow::bail!("unknown host: {host}. Supported: claude-code"),
    }
    println!("hooks registered for {host}. Restart the CLI to pick them up.");
    Ok(())
}

fn setup_claude_code() -> Result<()> {
    let home = std::env::var("HOME")?;
    let settings_path = std::path::PathBuf::from(&home)
        .join(".claude").join("settings.json");

    let raw = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)?
    } else {
        "{}".to_string()
    };

    let mut settings: serde_json::Map<String, Value> =
        serde_json::from_str(&raw).unwrap_or_default();

    let hooks = settings.entry("hooks").or_insert_with(|| Value::Object(Default::default()));
    let hooks = hooks.as_object_mut().unwrap();

    // Use the hooks directory next to this binary's source
    let hook_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hooks");

    let posttooluse_script = hook_dir.join("claude-posttooluse.sh").to_string_lossy().to_string();
    let promptsubmit_script = hook_dir.join("claude-promptsubmit.sh").to_string_lossy().to_string();

    hooks.insert("PostToolUse".to_string(), serde_json::json!([{
        "hooks": [{"type": "command", "command": posttooluse_script}]
    }]));
    hooks.insert("UserPromptSubmit".to_string(), serde_json::json!([{
        "hooks": [{"type": "command", "command": promptsubmit_script}]
    }]));

    std::fs::create_dir_all(settings_path.parent().unwrap())?;
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("wrote hooks to {}", settings_path.display());
    Ok(())
}

fn cmd_uninstall(args: &[String]) -> Result<()> {
    let host = args.iter()
        .find_map(|a| a.strip_prefix("--host="))
        .unwrap_or("claude-code");
    match host {
        "claude-code" => {
            let home = std::env::var("HOME")?;
            let settings_path = std::path::PathBuf::from(home)
                .join(".claude").join("settings.json");
            if !settings_path.exists() { return Ok(()); }
            let raw = std::fs::read_to_string(&settings_path)?;
            let mut settings: serde_json::Map<String, Value> =
                serde_json::from_str(&raw).unwrap_or_default();
            if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                hooks.remove("PostToolUse");
                hooks.remove("UserPromptSubmit");
            }
            std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
            println!("removed hooks from {}", settings_path.display());
        }
        _ => anyhow::bail!("unknown host: {host}"),
    }
    Ok(())
}

fn run_mcp(args: &[String]) -> Result<()> {
    let port: u16 = args.iter()
        .find_map(|a| a.strip_prefix("--port=").and_then(|p| p.parse().ok()))
        .unwrap_or(3456);
    mcp::server::run(port)?;
    Ok(())
}
