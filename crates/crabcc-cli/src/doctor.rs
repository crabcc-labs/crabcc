//! `crabcc doctor [subcommand]` — diagnostic surface, issue #107 / #105
//! Phase 5a.
//!
//! Operator-facing: each check returns a structured [`CheckResult`]
//! that the menubar app (issue #107 Part A) and Chrome extension
//! (issue #107 Part B) consume as JSON. The text formatter is for
//! humans running ` crabcc doctor` from a terminal.
//!
//! ## Surface (this branch)
//!
//! - ` crabcc doctor` — run all checks, aggregate to a [`DoctorReport`]
//! - ` crabcc doctor docker` — [`crabcc_core::ollama_stack::check_docker`] +
//!   OrbStack detection on macOS
//! - ` crabcc doctor stack` — Compose-stack health via
//!   [`crabcc_core::ollama_stack::status`]
//! - ` crabcc doctor keys` — inspect ` ~/.crabcc.local.api-key` and
//!   ` ~/.crabcc/ollama-stack/.env` mode + freshness
//!
//! Deferred to follow-up branches (each documented in issue #107):
//! - ` doctor stack init` / ` doctor stack up` / ` doctor stack repair`
//!   (the operational variants overlap with ` crabcc ollama-stack`)
//! - ` doctor agent` — needs a structured dry-run from agent.rs
//! - ` doctor extension` / ` doctor menubar` — wait on #107 Parts A/B
//! - ` doctor jobs` — waits on #109 BullMQ wire-protocol encoder

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    fn glyph(self) -> &'static str {
        match self {
            CheckStatus::Ok => "✓",
            CheckStatus::Warn => "!",
            CheckStatus::Fail => "✗",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub check: String,
    pub status: CheckStatus,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverallStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    pub overall: OverallStatus,
}

impl DoctorReport {
    fn from_checks(checks: Vec<CheckResult>) -> Self {
        let overall = if checks.iter().any(|c| c.status == CheckStatus::Fail) {
            OverallStatus::Fail
        } else if checks.iter().any(|c| c.status == CheckStatus::Warn) {
            OverallStatus::Warn
        } else {
            OverallStatus::Ok
        };
        Self { checks, overall }
    }
}

// ---------------------------------------------------------------------
// public entry points
// ---------------------------------------------------------------------

pub fn run_all(text: bool) -> Result<()> {
    let checks = vec![
        check_docker(),
        check_stack(),
        check_keys(),
        check_agent(),
        check_jobs(),
    ];
    let report = DoctorReport::from_checks(checks);
    if text {
        print_text_report(&report);
    } else {
        println!("{}", sonic_rs::to_string_pretty(&report)?);
    }
    if matches!(report.overall, OverallStatus::Fail) {
        std::process::exit(1);
    }
    Ok(())
}

pub fn run_docker(text: bool) -> Result<()> {
    emit_one(check_docker(), text)
}

pub fn run_stack(text: bool) -> Result<()> {
    emit_one(check_stack(), text)
}

pub fn run_keys(text: bool) -> Result<()> {
    emit_one(check_keys(), text)
}

pub fn run_agent(text: bool) -> Result<()> {
    emit_one(check_agent(), text)
}

pub fn run_jobs(text: bool) -> Result<()> {
    emit_one(check_jobs(), text)
}

fn emit_one(check: CheckResult, text: bool) -> Result<()> {
    let exiting_fail = check.status == CheckStatus::Fail;
    if text {
        print_text_check(&check);
    } else {
        println!("{}", sonic_rs::to_string_pretty(&check)?);
    }
    if exiting_fail {
        std::process::exit(1);
    }
    Ok(())
}

// ---------------------------------------------------------------------
// individual checks
// ---------------------------------------------------------------------

/// `docker --version` + `docker compose version` + OrbStack detection
/// (macOS). Maps to [`crabcc_core::ollama_stack::check_docker`] for
/// the verification, [`crabcc_core::ollama_stack::orbstack_available`]
/// for the runtime hint.
fn check_docker() -> CheckResult {
    use crabcc_core::ollama_stack as ols;
    match ols::check_docker() {
        Ok(()) => {
            let details = serde_json::json!({
                "docker": "ok",
                "compose": "ok",
                "orbstack_detected": ols::orbstack_available(),
            });
            CheckResult {
                check: "docker".into(),
                status: CheckStatus::Ok,
                details,
                hint: None,
            }
        }
        Err(e) => CheckResult {
            check: "docker".into(),
            status: CheckStatus::Fail,
            details: serde_json::json!({ "error": e.to_string() }),
            hint: Some(ols::install_hint()),
        },
    }
}

/// `crabcc_core::ollama_stack::status` — JSON array of running
/// containers. Empty array → Warn (stack not up). Error from the
/// driver → Fail. Compose dir missing → Fail with the same hint
/// the driver emits.
fn check_stack() -> CheckResult {
    use crabcc_core::ollama_stack as ols;
    let opts = ols::Options::new();
    // Probe compose-dir resolution first so a missing recipe doesn't
    // fall through into a confusing `docker compose ps` error.
    if let Err(e) = ols::resolve_compose_dir(&opts) {
        return CheckResult {
            check: "stack".into(),
            status: CheckStatus::Fail,
            details: serde_json::json!({ "error": e.to_string() }),
            hint: Some(
                "run `crabcc install-claude --with-ollama-stack` to seed the user-local copy"
                    .into(),
            ),
        };
    }
    match ols::status(&opts) {
        Ok(containers) if containers.is_empty() => CheckResult {
            check: "stack".into(),
            status: CheckStatus::Warn,
            details: serde_json::json!({ "containers": [] }),
            hint: Some("stack is not running — `crabcc ollama-stack up`".into()),
        },
        Ok(containers) => {
            let unhealthy: Vec<String> = containers
                .iter()
                .filter(|c| c.health.as_deref() == Some("unhealthy"))
                .map(|c| c.name.clone())
                .collect();
            let status = if unhealthy.is_empty() {
                CheckStatus::Ok
            } else {
                CheckStatus::Warn
            };
            let hint = if unhealthy.is_empty() {
                None
            } else {
                Some(format!(
                    "unhealthy containers: {} — try `crabcc ollama-stack logs <svc>`",
                    unhealthy.join(", ")
                ))
            };
            CheckResult {
                check: "stack".into(),
                status,
                details: serde_json::json!({
                    "containers": containers,
                    "unhealthy": unhealthy,
                }),
                hint,
            }
        }
        Err(e) => CheckResult {
            check: "stack".into(),
            status: CheckStatus::Fail,
            details: serde_json::json!({ "error": e.to_string() }),
            hint: Some("`crabcc ollama-stack status` for raw docker output".into()),
        },
    }
}

/// Inspect `~/.crabcc.local.api-key` and the auth-stack `.env`. Verifies
/// presence, file mode (0400 / 0600 expected), and writes a parity flag
/// when both files exist (the local key file should match the stack's
/// LITELLM_MASTER_KEY).
fn check_keys() -> CheckResult {
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => {
            return CheckResult {
                check: "keys".into(),
                status: CheckStatus::Fail,
                details: serde_json::json!({ "error": "HOME not set" }),
                hint: None,
            };
        }
    };
    let local_key = home.join(".crabcc.local.api-key");
    let stack_env = home.join(".crabcc/ollama-stack/.env");

    let local_info = inspect_key_file(&local_key, 0o400);
    let env_info = inspect_key_file(&stack_env, 0o600);

    let mut warnings: Vec<String> = Vec::new();
    let mut hint: Option<String> = None;
    let mut status = CheckStatus::Ok;

    if !local_key.exists() && !stack_env.exists() {
        status = CheckStatus::Warn;
        hint = Some(
            "no key files yet — run `crabcc install-claude --with-ollama-stack` then \
             `~/.crabcc/ollama-stack/init-keys.sh`"
                .into(),
        );
    } else {
        if !local_key.exists() {
            warnings.push("~/.crabcc.local.api-key missing".into());
        }
        if !stack_env.exists() {
            warnings.push("~/.crabcc/ollama-stack/.env missing".into());
        }
    }

    if let Some(mode) = local_info.get("mode_octal").and_then(|v| v.as_str()) {
        if mode != "400" {
            warnings.push(format!(
                "~/.crabcc.local.api-key mode is {mode}, expected 400 (chmod 400 to fix)"
            ));
            if !matches!(status, CheckStatus::Fail) {
                status = CheckStatus::Warn;
            }
        }
    }
    if let Some(mode) = env_info.get("mode_octal").and_then(|v| v.as_str()) {
        if mode != "600" {
            warnings.push(format!(
                "~/.crabcc/ollama-stack/.env mode is {mode}, expected 600"
            ));
            if !matches!(status, CheckStatus::Fail) {
                status = CheckStatus::Warn;
            }
        }
    }

    let in_sync = compare_master_key(&local_key, &stack_env);

    if !warnings.is_empty() && hint.is_none() {
        hint = Some(warnings.join("; "));
    }

    CheckResult {
        check: "keys".into(),
        status,
        details: serde_json::json!({
            "local_key": local_info,
            "env": env_info,
            "in_sync": in_sync,
            "warnings": warnings,
        }),
        hint,
    }
}

/// Probe the agent execution environment. Backend-agnostic — checks
/// for at least one usable runtime: a `claude` binary on PATH (Claude
/// Code backend) AND/OR `OLLAMA_BASE_URL`+`OLLAMA_API_KEY` env vars
/// for the Ollama backend. Doesn't actually invoke the agent — that
/// would burn tokens. Use `crabcc agent --dry-run` for richer probing.
fn check_agent() -> CheckResult {
    let claude_found = cmd_available("claude") || cmd_available("claude-code");
    let ollama_base_url = std::env::var("OLLAMA_BASE_URL").ok();
    let ollama_api_key_set = std::env::var("OLLAMA_API_KEY").is_ok();
    let ollama_ready = ollama_base_url.is_some() && ollama_api_key_set;

    let details = serde_json::json!({
        "claude_binary_on_path": claude_found,
        "ollama_base_url": ollama_base_url,
        "ollama_api_key_set": ollama_api_key_set,
        "ollama_ready": ollama_ready,
    });

    if !claude_found && !ollama_ready {
        return CheckResult {
            check: "agent".into(),
            status: CheckStatus::Fail,
            details,
            hint: Some(
                "no agent runtime available. Either install Claude Code (https://claude.com/claude-code) \
                 OR set OLLAMA_BASE_URL + OLLAMA_API_KEY (run `crabcc install-claude --with-ollama-stack`)"
                    .into(),
            ),
        };
    }

    if !claude_found {
        return CheckResult {
            check: "agent".into(),
            status: CheckStatus::Warn,
            details,
            hint: Some(
                "claude binary missing — only `crabcc agent --backend ollama` will work \
                 (use `--backend claude` requires Claude Code installed)"
                    .into(),
            ),
        };
    }

    CheckResult {
        check: "agent".into(),
        status: CheckStatus::Ok,
        details,
        hint: None,
    }
}

/// Probe Redis reachability for the BullMQ-backed jobs surface (issue
/// #109). Shells out to `redis-cli ping` against `$REDIS_URL` (default
/// `redis://127.0.0.1:6379`) — no Rust redis dep needed for this
/// diagnostic check, keeps the binary lean.
fn check_jobs() -> CheckResult {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    if !cmd_available("redis-cli") {
        return CheckResult {
            check: "jobs".into(),
            status: CheckStatus::Warn,
            details: serde_json::json!({
                "redis_url": redis_url,
                "redis_cli_on_path": false,
            }),
            hint: Some(
                "redis-cli missing — install via `brew install redis` (macOS) or \
                 `apt install redis-tools` (Debian/Ubuntu). Without it the doctor \
                 can't probe the jobs queue. Stack itself runs fine without."
                    .into(),
            ),
        };
    }

    let out = std::process::Command::new("redis-cli")
        .arg("-u")
        .arg(&redis_url)
        .arg("ping")
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            let pong = body.trim() == "PONG";
            CheckResult {
                check: "jobs".into(),
                status: if pong {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Warn
                },
                details: serde_json::json!({
                    "redis_url": redis_url,
                    "redis_cli_on_path": true,
                    "ping_response": body.trim(),
                }),
                hint: if pong {
                    None
                } else {
                    Some(format!("unexpected redis ping response: {}", body.trim()))
                },
            }
        }
        Ok(o) => CheckResult {
            check: "jobs".into(),
            status: CheckStatus::Warn,
            details: serde_json::json!({
                "redis_url": redis_url,
                "redis_cli_on_path": true,
                "exit_code": o.status.code(),
                "stderr": String::from_utf8_lossy(&o.stderr).to_string(),
            }),
            hint: Some(
                "redis not reachable — bring it up: \
                 `docker compose -f install/dev/docker-compose.yml --profile jobs up -d`"
                    .into(),
            ),
        },
        Err(e) => CheckResult {
            check: "jobs".into(),
            status: CheckStatus::Fail,
            details: serde_json::json!({
                "redis_url": redis_url,
                "spawn_error": e.to_string(),
            }),
            hint: None,
        },
    }
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

/// Lightweight binary-on-PATH probe — runs `<name> --version` and
/// checks the exit code. ~5 ms when the binary exists, ~1 ms when it
/// doesn't (PATH scan + early exec failure).
fn cmd_available(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn inspect_key_file(path: &std::path::Path, _expected_mode: u32) -> serde_json::Value {
    if !path.exists() {
        return serde_json::json!({ "path": path.display().to_string(), "exists": false });
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return serde_json::json!({ "error": e.to_string() }),
    };
    let mode_str = file_mode_octal(&meta);
    serde_json::json!({
        "path": path.display().to_string(),
        "exists": true,
        "size_bytes": meta.len(),
        "mode_octal": mode_str,
    })
}

fn file_mode_octal(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        format!("{:o}", meta.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        "(non-unix)".into()
    }
}

/// Compare the master key in the local file vs the LITELLM_MASTER_KEY
/// line in the stack's .env. Returns Some(true/false) when both files
/// exist; None when either is missing.
fn compare_master_key(local: &std::path::Path, env: &std::path::Path) -> Option<bool> {
    let local_body = std::fs::read_to_string(local).ok()?;
    let env_body = std::fs::read_to_string(env).ok()?;
    let local_key = local_body.lines().next()?.trim();
    let env_key = env_body
        .lines()
        .find_map(|l| l.strip_prefix("LITELLM_MASTER_KEY="))
        .map(str::trim)?;
    Some(local_key == env_key)
}

// ---------------------------------------------------------------------
// text formatter
// ---------------------------------------------------------------------

fn print_text_report(report: &DoctorReport) {
    println!("crabcc doctor — {} checks", report.checks.len());
    for c in &report.checks {
        print_text_check(c);
    }
    let label = match report.overall {
        OverallStatus::Ok => "✓ all good",
        OverallStatus::Warn => "! warnings present",
        OverallStatus::Fail => "✗ failures present",
    };
    println!();
    println!("overall: {label}");
}

fn print_text_check(c: &CheckResult) {
    println!("  {} {}", c.status.glyph(), c.check);
    if let Some(h) = &c.hint {
        println!("    hint: {h}");
    }
}

// ---------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn check_status_glyphs_distinct() {
        assert_ne!(CheckStatus::Ok.glyph(), CheckStatus::Warn.glyph());
        assert_ne!(CheckStatus::Warn.glyph(), CheckStatus::Fail.glyph());
    }

    #[test]
    fn report_overall_ok_when_all_ok() {
        let checks = vec![CheckResult {
            check: "x".into(),
            status: CheckStatus::Ok,
            details: Value::Null,
            hint: None,
        }];
        let r = DoctorReport::from_checks(checks);
        assert_eq!(r.overall, OverallStatus::Ok);
    }

    #[test]
    fn report_overall_warn_when_any_warn() {
        let checks = vec![
            CheckResult {
                check: "a".into(),
                status: CheckStatus::Ok,
                details: Value::Null,
                hint: None,
            },
            CheckResult {
                check: "b".into(),
                status: CheckStatus::Warn,
                details: Value::Null,
                hint: None,
            },
        ];
        let r = DoctorReport::from_checks(checks);
        assert_eq!(r.overall, OverallStatus::Warn);
    }

    #[test]
    fn report_overall_fail_when_any_fail() {
        let checks = vec![
            CheckResult {
                check: "a".into(),
                status: CheckStatus::Warn,
                details: Value::Null,
                hint: None,
            },
            CheckResult {
                check: "b".into(),
                status: CheckStatus::Fail,
                details: Value::Null,
                hint: None,
            },
        ];
        let r = DoctorReport::from_checks(checks);
        assert_eq!(r.overall, OverallStatus::Fail);
    }

    #[test]
    fn check_status_serializes_snake_case() {
        let s = serde_json::to_string(&CheckStatus::Ok).unwrap();
        assert_eq!(s, "\"ok\"");
        let s = serde_json::to_string(&CheckStatus::Fail).unwrap();
        assert_eq!(s, "\"fail\"");
    }

    #[test]
    fn check_result_serializes_with_optional_hint_skipped() {
        let c = CheckResult {
            check: "test".into(),
            status: CheckStatus::Ok,
            details: serde_json::json!({"x": 1}),
            hint: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("hint"));
    }

    #[test]
    fn inspect_key_file_reports_missing() {
        let v = inspect_key_file(std::path::Path::new("/nonexistent/path/xyz"), 0o400);
        assert_eq!(v.get("exists").and_then(|x| x.as_bool()), Some(false));
    }

    #[test]
    fn compare_master_key_returns_none_for_missing_files() {
        let r = compare_master_key(
            std::path::Path::new("/nope-1"),
            std::path::Path::new("/nope-2"),
        );
        assert!(r.is_none());
    }

    #[test]
    fn compare_master_key_detects_match() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let env = dir.path().join("env");
        std::fs::write(&local, "sk-test123\n").unwrap();
        let mut f = std::fs::File::create(&env).unwrap();
        writeln!(f, "OLLAMA_API_KEY=other").unwrap();
        writeln!(f, "LITELLM_MASTER_KEY=sk-test123").unwrap();
        assert_eq!(compare_master_key(&local, &env), Some(true));
    }

    #[test]
    fn compare_master_key_detects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let env = dir.path().join("env");
        std::fs::write(&local, "sk-aaa\n").unwrap();
        std::fs::write(&env, "LITELLM_MASTER_KEY=sk-bbb\n").unwrap();
        assert_eq!(compare_master_key(&local, &env), Some(false));
    }

    #[test]
    #[ignore = "shells out to `cargo --version`; defensive ignore for minimal CI containers — run locally with --ignored"]
    fn cmd_available_returns_true_for_real_binary() {
        // `cargo` is guaranteed on PATH for any Rust CI run AND
        // supports `--version`. Don't switch back to `sh` — Ubuntu's
        // `/bin/sh` is dash, which doesn't accept `--version` and
        // exits non-zero, defeating the assertion.
        assert!(cmd_available("cargo"));
    }

    #[test]
    #[ignore = "shells out probing PATH; defensive ignore for minimal CI containers — run locally with --ignored"]
    fn cmd_available_returns_false_for_missing_binary() {
        assert!(!cmd_available("definitely-not-a-real-binary-xyz-9876543"));
    }

    #[test]
    fn check_agent_fails_when_no_runtime() {
        // Force ollama env unset for this test only.
        let prev_url = std::env::var_os("OLLAMA_BASE_URL");
        let prev_key = std::env::var_os("OLLAMA_API_KEY");
        std::env::remove_var("OLLAMA_BASE_URL");
        std::env::remove_var("OLLAMA_API_KEY");

        let r = check_agent();
        // Status depends on whether `claude` happens to be on the host PATH.
        // We can only assert the surface fields exist.
        assert_eq!(r.check, "agent");
        assert!(r.details.get("ollama_ready").is_some());

        if let Some(url) = prev_url {
            std::env::set_var("OLLAMA_BASE_URL", url);
        }
        if let Some(key) = prev_key {
            std::env::set_var("OLLAMA_API_KEY", key);
        }
    }

    #[test]
    fn check_jobs_returns_warn_when_redis_cli_missing() {
        // We can't *un*-install redis-cli for the test, so we can only
        // assert the surface contract: `check` field, `redis_url` echoed.
        let r = check_jobs();
        assert_eq!(r.check, "jobs");
        assert!(r
            .details
            .get("redis_url")
            .and_then(|v| v.as_str())
            .is_some());
    }
}
