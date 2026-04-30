//! Driver for the bundled Ollama auth Compose stack — issue #105.
//!
//! Wraps `docker` / `docker compose` invocations against the stack at
//! `install/ollama-stack/` (or a user-local copy seeded by `crabcc
//! install-claude --with-ollama-stack`). Pure `std::process::Command` —
//! no Docker SDK dependency.
//!
//! ## Resolution chain (compose dir)
//!
//! 1. Explicit [`Options::compose_dir`].
//! 2. `$CRABCC_OLLAMA_STACK_DIR` env var.
//! 3. `$HOME/.crabcc/ollama-stack/`.
//!
//! If none of the above resolve to a directory containing `docker-compose.yml`,
//! the caller gets a [`Error::ComposeMissing`] with a one-line install hint.
//!
//! ## Tracing surface
//!
//! All calls emit JSON-shaped events under
//! `target = "crabcc_core::ollama_stack"` for the `/live` viz and
//! `agent.rs` debug-log consumers. Six event discriminators (carried in
//! the `event` field):
//!
//! - `ollama_stack.detect`         — pre-flight detection of the stack
//! - `ollama_stack.up.start`       — compose up began
//! - `ollama_stack.up.done`        — compose up returned, healthchecks green
//! - `ollama_stack.container_info` — one event per running container
//! - `ollama_stack.probe`          — LiteLLM `/v1/models` smoke test
//! - `ollama_stack.error`          — any failed phase, with `stderr_tail`
//!
//! Pass [`Options::correlation_id`] to thread an `x-Request-ID` through
//! a multi-step flow ([`ensure_up`] does this internally).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

const ENV_OVERRIDE: &str = "CRABCC_OLLAMA_STACK_DIR";
const USER_LOCAL_SUBDIR: &str = ".crabcc/ollama-stack";
const COMPOSE_FILENAME: &str = "docker-compose.yml";
const TRACE_TARGET: &str = "crabcc_core::ollama_stack";

/// Caller-provided knobs. Use [`Options::default`] for the common case.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Override the compose directory. Highest precedence.
    pub compose_dir: Option<PathBuf>,
    /// Threaded through the tracing `request_id` field. None = generated
    /// per-call.
    pub correlation_id: Option<String>,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_compose_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.compose_dir = Some(dir.into());
        self
    }
    pub fn with_correlation_id<S: Into<String>>(mut self, id: S) -> Self {
        self.correlation_id = Some(id.into());
        self
    }
}

/// Result of [`detect`] — what we found on disk, no docker calls beyond
/// `compose ps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackInfo {
    pub compose_file: PathBuf,
    pub services_total: usize,
    pub services_running: usize,
    pub services: Vec<String>,
}

/// One row per running container. Mirrors the
/// `ollama_stack.container_info` tracing event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    pub container_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    pub ports: Vec<String>,
    pub created: String,
    pub restart_count: u32,
    pub networks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpResult {
    pub duration_ms: u64,
    pub services_healthy: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub url: String,
    pub http_status: u16,
    pub latency_ms: u64,
    pub models_count: usize,
}

// ---------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------

/// Verify `docker --version` and `docker compose version` both exit 0.
/// Returns a one-line install hint on failure.
pub fn check_docker() -> Result<()> {
    let v = Command::new("docker")
        .arg("--version")
        .output()
        .with_context(|| {
            "Docker CLI not found in PATH — install Docker 24+ from \
         https://docs.docker.com/get-docker/"
        })?;
    if !v.status.success() {
        return Err(anyhow!(
            "`docker --version` exited {}: {}",
            v.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&v.stderr).trim()
        ));
    }
    let c = Command::new("docker")
        .args(["compose", "version"])
        .output()
        .context("Docker Compose v2 plugin missing — re-install Docker or `apt install docker-compose-plugin`")?;
    if !c.status.success() {
        return Err(anyhow!(
            "`docker compose version` exited {}: {}",
            c.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&c.stderr).trim()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Compose-dir resolution
// ---------------------------------------------------------------------

/// Resolve the compose directory in priority order. Pure — no docker calls.
pub fn resolve_compose_dir(opts: &Options) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = &opts.compose_dir {
        candidates.push(d.clone());
    }
    if let Ok(d) = std::env::var(ENV_OVERRIDE) {
        candidates.push(PathBuf::from(d));
    }
    if let Some(home) = home_dir() {
        candidates.push(home.join(USER_LOCAL_SUBDIR));
    }

    for cand in &candidates {
        if cand.join(COMPOSE_FILENAME).is_file() {
            return Ok(cand.clone());
        }
    }

    Err(anyhow!(
        "no Ollama Compose stack found at any of: {}; \
         seed it with `crabcc install-claude --with-ollama-stack` (issue #105)",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ---------------------------------------------------------------------
// detect
// ---------------------------------------------------------------------

/// Resolve the stack dir, list services from `compose config --services`,
/// and count running services from `compose ps`. No mutation.
pub fn detect(opts: &Options) -> Result<StackInfo> {
    let dir = resolve_compose_dir(opts)?;
    let compose_file = dir.join(COMPOSE_FILENAME);
    let cid = correlation(opts);

    let services_out = run_compose(&dir, &["config", "--services"], "detect:config", &cid)?;
    let services: Vec<String> = String::from_utf8_lossy(&services_out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let ps_out = run_compose(&dir, &["ps", "--format", "json"], "detect:ps", &cid)?;
    let services_running = String::from_utf8_lossy(&ps_out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();

    let info = StackInfo {
        compose_file,
        services_total: services.len(),
        services_running,
        services,
    };

    tracing::info!(
        target: TRACE_TARGET,
        event = "ollama_stack.detect",
        request_id = %cid,
        compose_file = %info.compose_file.display(),
        services_total = info.services_total,
        services_running = info.services_running,
        "stack detect"
    );

    Ok(info)
}

// ---------------------------------------------------------------------
// up
// ---------------------------------------------------------------------

/// `docker compose up -d --wait` against the resolved stack. Blocks until
/// every healthcheck passes or compose times out.
pub fn up(opts: &Options) -> Result<UpResult> {
    check_docker()?;
    let dir = resolve_compose_dir(opts)?;
    let cid = correlation(opts);
    let info = detect(opts)?;

    tracing::info!(
        target: TRACE_TARGET,
        event = "ollama_stack.up.start",
        request_id = %cid,
        compose_file = %dir.join(COMPOSE_FILENAME).display(),
        services = ?info.services,
        "stack up start"
    );

    let start = Instant::now();
    let out = run_compose(&dir, &["up", "-d", "--wait"], "up", &cid)?;
    let duration_ms = start.elapsed().as_millis() as u64;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        emit_error(&cid, "up", out.status.code().unwrap_or(-1), &stderr);
        return Err(anyhow!(
            "docker compose up -d --wait failed: {}",
            stderr_tail(&stderr, 800)
        ));
    }

    let services_healthy = info.services.clone();

    tracing::info!(
        target: TRACE_TARGET,
        event = "ollama_stack.up.done",
        request_id = %cid,
        duration_ms,
        services_healthy = ?services_healthy,
        "stack up done"
    );

    // Best-effort: emit one container_info event per running container.
    // Failures here don't mask up's success.
    if let Ok(containers) = status(opts) {
        for c in &containers {
            tracing::info!(
                target: TRACE_TARGET,
                event = "ollama_stack.container_info",
                request_id = %cid,
                name = %c.name,
                image = %c.image,
                image_digest = ?c.image_digest,
                container_id = %c.container_id,
                status = %c.status,
                health = ?c.health,
                ports = ?c.ports,
                created = %c.created,
                restart_count = c.restart_count,
                networks = ?c.networks,
                "stack container info"
            );
        }
    }

    Ok(UpResult {
        duration_ms,
        services_healthy,
    })
}

// ---------------------------------------------------------------------
// down / pull
// ---------------------------------------------------------------------

pub fn down(opts: &Options, with_volumes: bool) -> Result<Vec<String>> {
    let dir = resolve_compose_dir(opts)?;
    let cid = correlation(opts);
    let info = detect(opts)?;
    let mut args: Vec<&str> = vec!["down"];
    if with_volumes {
        args.push("--volumes");
    }
    let out = run_compose(&dir, &args, "down", &cid)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        emit_error(&cid, "down", out.status.code().unwrap_or(-1), &stderr);
        return Err(anyhow!(
            "docker compose down failed: {}",
            stderr_tail(&stderr, 800)
        ));
    }
    Ok(info.services)
}

pub fn pull(opts: &Options) -> Result<()> {
    let dir = resolve_compose_dir(opts)?;
    let cid = correlation(opts);
    let out = run_compose(&dir, &["pull"], "pull", &cid)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        emit_error(&cid, "pull", out.status.code().unwrap_or(-1), &stderr);
        return Err(anyhow!(
            "docker compose pull failed: {}",
            stderr_tail(&stderr, 800)
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// status — combines `compose ps --format json` + `docker inspect`
// ---------------------------------------------------------------------

pub fn status(opts: &Options) -> Result<Vec<ContainerInfo>> {
    let dir = resolve_compose_dir(opts)?;
    let cid = correlation(opts);
    let out = run_compose(&dir, &["ps", "--format", "json"], "status:ps", &cid)?;
    let stdout = String::from_utf8_lossy(&out.stdout);

    let mut ids: Vec<String> = Vec::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(id) = v.get("ID").and_then(|x| x.as_str()) {
            ids.push(id.to_string());
        }
    }

    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut inspect_args: Vec<String> = vec!["inspect".into()];
    inspect_args.extend(ids.iter().cloned());
    let out = Command::new("docker")
        .args(&inspect_args)
        .output()
        .context("docker inspect failed to spawn")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        emit_error(
            &cid,
            "status:inspect",
            out.status.code().unwrap_or(-1),
            &stderr,
        );
        return Err(anyhow!(
            "docker inspect failed: {}",
            stderr_tail(&stderr, 800)
        ));
    }

    let docs: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).context("docker inspect emitted non-JSON output")?;
    Ok(docs.into_iter().map(parse_container).collect())
}

fn parse_container(v: serde_json::Value) -> ContainerInfo {
    let name = v
        .get("Name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim_start_matches('/')
        .to_string();
    let image = v
        .get("Config")
        .and_then(|c| c.get("Image"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let image_digest = v.get("Image").and_then(|x| x.as_str()).map(str::to_string);
    let container_id = v
        .get("Id")
        .and_then(|x| x.as_str())
        .map(|s| s.chars().take(12).collect())
        .unwrap_or_default();
    let state = v.get("State");
    let status = state
        .and_then(|s| s.get("Status"))
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let health = state
        .and_then(|s| s.get("Health"))
        .and_then(|h| h.get("Status"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let restart_count = v.get("RestartCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let created = v
        .get("Created")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let ports = v
        .get("NetworkSettings")
        .and_then(|n| n.get("Ports"))
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let networks = v
        .get("NetworkSettings")
        .and_then(|n| n.get("Networks"))
        .and_then(|p| p.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    ContainerInfo {
        name,
        image,
        image_digest,
        container_id,
        status,
        health,
        ports,
        created,
        restart_count,
        networks,
    }
}

// ---------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------

pub fn logs(opts: &Options, service: Option<&str>, tail: usize) -> Result<String> {
    let dir = resolve_compose_dir(opts)?;
    let cid = correlation(opts);
    let tail_str = tail.to_string();
    let mut args: Vec<&str> = vec!["logs", "--no-color", "--tail", &tail_str];
    if let Some(s) = service {
        args.push(s);
    }
    let out = run_compose(&dir, &args, "logs", &cid)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        emit_error(&cid, "logs", out.status.code().unwrap_or(-1), &stderr);
        return Err(anyhow!(
            "docker compose logs failed: {}",
            stderr_tail(&stderr, 800)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------
// probe — LiteLLM /v1/models smoke
// ---------------------------------------------------------------------

/// Probe the LiteLLM proxy at `base_url/v1/models` with the master key.
/// Shells out to `curl` (already required for LiteLLM's healthcheck) so we
/// avoid pulling in a Rust HTTP client just for this one call.
pub fn probe(opts: &Options, base_url: &str, master_key: &str) -> Result<ProbeResult> {
    let cid = correlation(opts);
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let auth = format!("Authorization: Bearer {master_key}");
    let start = Instant::now();
    let out = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "-o",
            "/dev/stdout",
            "-w",
            "%{http_code}",
            "-H",
            &auth,
            &url,
        ])
        .output()
        .context("curl not found in PATH; required for LiteLLM probe")?;
    let latency_ms = start.elapsed().as_millis() as u64;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        emit_error(&cid, "probe", out.status.code().unwrap_or(-1), &stderr);
        return Err(anyhow!(
            "LiteLLM probe failed: {}",
            stderr_tail(&stderr, 400)
        ));
    }

    let body = String::from_utf8_lossy(&out.stdout);
    // Last 3 chars are the HTTP status from `-w %{http_code}`.
    let (json_body, status_str) = if body.len() >= 3 {
        body.split_at(body.len() - 3)
    } else {
        ("", body.as_ref())
    };
    let http_status: u16 = status_str.trim().parse().unwrap_or(0);

    let models_count = serde_json::from_str::<serde_json::Value>(json_body)
        .ok()
        .and_then(|v| v.get("data").and_then(|d| d.as_array()).map(|a| a.len()))
        .unwrap_or(0);

    let result = ProbeResult {
        url,
        http_status,
        latency_ms,
        models_count,
    };

    tracing::info!(
        target: TRACE_TARGET,
        event = "ollama_stack.probe",
        request_id = %cid,
        url = %result.url,
        http_status = result.http_status,
        latency_ms = result.latency_ms,
        models_count = result.models_count,
        "stack probe"
    );

    Ok(result)
}

// ---------------------------------------------------------------------
// ensure_up — convenience for callers (agent.rs)
// ---------------------------------------------------------------------

/// Detect → up if not running → status. Idempotent: cheap when already up.
/// Does NOT probe — caller decides whether to follow with [`probe`] (needs
/// the master-key, which only the caller has).
pub fn ensure_up(opts: &Options) -> Result<UpResult> {
    let info = detect(opts)?;
    if info.services_running >= info.services_total && info.services_total > 0 {
        // Already up — emit container_info events anyway so consumers see
        // the current state.
        if let Ok(containers) = status(opts) {
            let cid = correlation(opts);
            for c in &containers {
                tracing::info!(
                    target: TRACE_TARGET,
                    event = "ollama_stack.container_info",
                    request_id = %cid,
                    name = %c.name,
                    image = %c.image,
                    container_id = %c.container_id,
                    status = %c.status,
                    health = ?c.health,
                    "stack already-up container info"
                );
            }
        }
        return Ok(UpResult {
            duration_ms: 0,
            services_healthy: info.services,
        });
    }
    up(opts)
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

fn run_compose(dir: &Path, args: &[&str], phase: &str, cid: &str) -> Result<Output> {
    let mut cmd = Command::new("docker");
    cmd.arg("compose").current_dir(dir);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().with_context(|| {
        format!(
            "docker compose {} failed to spawn (phase={phase})",
            args.join(" ")
        )
    })?;

    tracing::debug!(
        target: TRACE_TARGET,
        request_id = %cid,
        phase,
        args = ?args,
        exit = out.status.code().unwrap_or(-1),
        stdout_bytes = out.stdout.len(),
        stderr_bytes = out.stderr.len(),
        "compose call"
    );

    Ok(out)
}

fn emit_error(cid: &str, phase: &str, exit_code: i32, stderr: &str) {
    tracing::warn!(
        target: TRACE_TARGET,
        event = "ollama_stack.error",
        request_id = %cid,
        phase,
        exit_code,
        stderr_tail = %stderr_tail(stderr, 1000),
        "stack error"
    );
}

fn stderr_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s.chars()
            .rev()
            .take(max)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }
}

fn correlation(opts: &Options) -> String {
    opts.correlation_id.clone().unwrap_or_else(|| {
        // Cheap pseudo-id: nanos-since-epoch in hex. Sufficient for log
        // correlation; not security-relevant.
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("ols-{nanos:x}")
    })
}

// ---------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_minimal_compose(dir: &Path) {
        std::fs::write(
            dir.join(COMPOSE_FILENAME),
            "name: t\nservices:\n  a:\n    image: hello-world\n",
        )
        .unwrap();
    }

    #[test]
    fn resolve_explicit_override_wins() {
        let td = TempDir::new().unwrap();
        write_minimal_compose(td.path());
        let opts = Options::new().with_compose_dir(td.path());
        let resolved = resolve_compose_dir(&opts).unwrap();
        assert_eq!(resolved, td.path());
    }

    #[test]
    fn resolve_env_override_picked_when_no_explicit() {
        let td = TempDir::new().unwrap();
        write_minimal_compose(td.path());
        std::env::set_var(ENV_OVERRIDE, td.path());
        let opts = Options::new();
        let resolved = resolve_compose_dir(&opts).unwrap();
        assert_eq!(resolved, td.path());
        std::env::remove_var(ENV_OVERRIDE);
    }

    #[test]
    fn resolve_errors_when_nothing_resolves() {
        let td = TempDir::new().unwrap();
        // No compose file inside td — ensure error.
        std::env::set_var(ENV_OVERRIDE, td.path());
        // Unset HOME so the third-tier lookup also fails.
        let prev_home = std::env::var_os("HOME");
        std::env::remove_var("HOME");
        let opts = Options::new();
        let err = resolve_compose_dir(&opts).unwrap_err();
        assert!(
            err.to_string()
                .contains("install-claude --with-ollama-stack"),
            "expected install-claude hint, got: {err}"
        );
        std::env::remove_var(ENV_OVERRIDE);
        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        }
    }

    #[test]
    fn stderr_tail_truncates_long_input() {
        let s: String = std::iter::repeat('x').take(2000).collect();
        let t = stderr_tail(&s, 100);
        assert_eq!(t.len(), 100);
        assert!(t.chars().all(|c| c == 'x'));
    }

    #[test]
    fn stderr_tail_passthrough_short_input() {
        let s = "boom";
        assert_eq!(stderr_tail(s, 100), "boom");
    }

    #[test]
    fn parse_container_extracts_minimal_fields() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "Name": "/crabcc-ollama-stack-ollama-1",
                "Id": "abcdef0123456789",
                "Image": "sha256:deadbeef",
                "Config": { "Image": "ollama/ollama:latest" },
                "RestartCount": 0,
                "Created": "2026-04-30T10:00:00Z",
                "State": { "Status": "running", "Health": { "Status": "healthy" } },
                "NetworkSettings": {
                  "Ports": { "11434/tcp": null },
                  "Networks": { "stack": {} }
                }
              }"#,
        )
        .unwrap();
        let c = parse_container(v);
        assert_eq!(c.name, "crabcc-ollama-stack-ollama-1");
        assert_eq!(c.image, "ollama/ollama:latest");
        assert_eq!(c.image_digest.as_deref(), Some("sha256:deadbeef"));
        assert_eq!(c.container_id, "abcdef012345");
        assert_eq!(c.status, "running");
        assert_eq!(c.health.as_deref(), Some("healthy"));
        assert_eq!(c.ports, vec!["11434/tcp"]);
        assert_eq!(c.networks, vec!["stack"]);
    }

    #[test]
    fn correlation_uses_supplied_id() {
        let opts = Options::new().with_correlation_id("test-cid");
        assert_eq!(correlation(&opts), "test-cid");
    }

    #[test]
    fn correlation_generates_when_unset() {
        let opts = Options::new();
        let cid = correlation(&opts);
        assert!(cid.starts_with("ols-"));
        assert!(cid.len() > 4);
    }
}
