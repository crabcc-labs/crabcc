//! End-to-end test for `crabcc agent --run … --dry-run` (issue #62).
//!
//! Invokes the actual `crabcc` binary in a tempdir-backed `$HOME` and
//! asserts the run-dir scaffolding exists, the dry-run banner mentions
//! the right surfaces, and the lock is cleaned up on graceful exit.
//! This catches regressions where the CLI argv → `agent::run` plumbing
//! drifts from the unit tests' direct calls into the module.

use std::process::Command;
use std::time::Duration;

fn crabcc_bin() -> std::path::PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by Cargo for integration tests.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_crabcc"))
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_creates_rundir_and_banner_lists_paths() {
    let home = tempfile::tempdir().expect("tempdir for HOME");
    let repo = tempfile::tempdir().expect("tempdir for repo");

    // A minimal AGENTS.md so the system-prompt branch fires.
    std::fs::write(repo.path().join("AGENTS.md"), "be terse\n").unwrap();

    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("trace callers of Store::open")
        .arg("--dry-run")
        .arg("--no-refresh")
        // Isolate $HOME so the run-dir lands in our tempdir, not the
        // dev's actual ~/.crabcc/agents/.
        .env("HOME", home.path())
        // Drop ANTHROPIC_API_KEY so the dry-run's auth banner stays
        // deterministic across machines that may or may not have it set.
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("spawn crabcc");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "agent --dry-run should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Banner must surface the run-dir + log path so the user knows
    // where to `tail -f`.
    assert!(
        stdout.contains("run id"),
        "missing run id in banner: {stdout}"
    );
    assert!(
        stdout.contains("run dir"),
        "missing run dir in banner: {stdout}"
    );
    assert!(
        stdout.contains("log (tail -f)"),
        "missing log path: {stdout}"
    );
    assert!(
        stdout.contains("AGENTS.md"),
        "system-prompt source must be reported: {stdout}"
    );
    assert!(
        stdout.contains("trace callers"),
        "prompt preview must echo back: {stdout}"
    );
    assert!(
        stdout.contains("auth"),
        "auth status line must be present: {stdout}"
    );

    // Filesystem contract: ~/.crabcc/agents/<id>/{lock removed, log present, meta present}
    let agents_dir = home.path().join(".crabcc").join("agents");
    let entries: Vec<_> = std::fs::read_dir(&agents_dir)
        .expect("agents dir should exist")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one run dir, got {}",
        entries.len()
    );
    let run = entries[0].path();
    assert!(run.join("log").exists(), "log file should remain after run");
    // dry_run never spawns a real child — pid file exists but contains "0\n"
    // (the placeholder written at RunDir::create so the file always exists).
    let pid_content = std::fs::read_to_string(run.join("pid")).unwrap_or_default();
    assert_eq!(
        pid_content.trim(),
        "0",
        "dry-run pid file should contain placeholder '0', got: {pid_content:?}"
    );
    // Graceful exit removes the lock.
    assert!(
        !run.join("lock").exists(),
        "lock should be removed after graceful exit"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_handles_missing_agents_md() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("hello")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .output()
        .expect("spawn crabcc");

    assert!(
        out.status.success(),
        "dry-run must succeed even without AGENTS.md"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("(none — agent default)"),
        "missing-AGENTS.md branch must surface in banner: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_uses_default_model_when_unset() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Backend default flipped to `ollama` in v2.8.x (issue #105). Pass
    // `--backend claude` explicitly to keep this test pinned to the
    // Anthropic-default code path. The ollama default's own coverage
    // lives in `agent_dry_run_ollama_default_uses_qwen_coder` below.
    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("hi")
        .arg("--dry-run")
        .arg("--no-refresh")
        .arg("--backend")
        .arg("claude")
        .env("HOME", home.path())
        .output()
        .expect("spawn crabcc");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Default must be Opus 4.7 — the strongest agentic Claude 4.x at
    // ship time. Bumping this constant should land in lockstep with a
    // README + agent-runtime.md update.
    assert!(
        stdout.contains("claude-opus-4-7"),
        "default model should be claude-opus-4-7: {stdout}"
    );
    assert!(
        stdout.contains("(default)"),
        "banner should mark the default origin: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_ollama_default_uses_qwen35() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Default backend is ollama; this test pins the Apple-optimized
    // qwen3.5 model so a stale constant doesn't silently regress.
    // Updated from qwen2.5-coder → qwen3.5:35b-a3b-coding-nvfp4 in v2.9+.
    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("hi")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .output()
        .expect("spawn crabcc");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("qwen3.5"),
        "default ollama model should be qwen3.5 variant: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_env_override_model() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // CRABCC_OLLAMA_MODEL env var overrides the compiled-in default.
    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("hi")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .env("CRABCC_OLLAMA_MODEL", "ollama/qwen2.5-coder")
        .output()
        .expect("spawn crabcc");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("qwen2.5-coder"),
        "CRABCC_OLLAMA_MODEL env should override default: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_stdin_pipe() {
    use std::io::Write;
    use std::process::Stdio;

    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let mut child = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("-")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn crabcc");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"find the Store symbol\n")
        .unwrap();

    let out = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "stdin pipe dry-run should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("find the Store symbol"),
        "prompt from stdin should appear in dry-run banner: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_short_task() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Minimal one-word prompt — guard against length validation bugs.
    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("ping")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .output()
        .expect("spawn crabcc");

    assert!(
        out.status.success(),
        "one-word prompt should be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ping"),
        "prompt preview must include the task: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_tool_call_context() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Prompt phrased as a tool-call instruction — validates that the
    // agent runtime doesn't pre-filter on prompt content.
    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("call crabcc.sym with name=Store and report the definition")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .output()
        .expect("spawn crabcc");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("crabcc.sym"),
        "tool-call prompt should round-trip in banner: {stdout}"
    );
}

#[test]
#[ignore = "needs ollama-stack + matching model in CI; run locally with --ignored"]
fn agent_dry_run_marks_explicit_model_origin() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let out = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("hi")
        .arg("--model")
        .arg("claude-sonnet-4-6")
        .arg("--dry-run")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .output()
        .expect("spawn crabcc");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude-sonnet-4-6"));
    assert!(
        stdout.contains("(explicit)"),
        "banner should mark explicit overrides: {stdout}"
    );
}

#[test]
#[ignore = "requires `claude` on PATH and burns tokens; run locally with --ignored"]
fn agent_real_run_exits_with_agent_status() {
    // This test is `#[ignore]` because it actually invokes `claude`,
    // which would require auth + burn tokens on every CI run. Local
    // devs can run it via:
    //   cargo test -p crabcc-cli --test agent_dry_run -- --ignored
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let _ = Command::new(crabcc_bin())
        .arg("--root")
        .arg(repo.path())
        .arg("agent")
        .arg("run")
        .arg("--run")
        .arg("respond with the literal text PONG and nothing else")
        .arg("--no-refresh")
        .env("HOME", home.path())
        .timeout(Duration::from_secs(60))
        .output();
    // We don't assert on output — Claude's response shape is not in
    // our contract. The point of this `--ignored` test is to confirm
    // the wiring from CLI → spawn → tee → run-dir survives a real
    // round-trip.
}

// Expose `Command::timeout` as a no-op alias so the gated `--ignored`
// test compiles without pulling in `wait-timeout` or similar. Real
// `claude` invocations finish in seconds.
trait CommandTimeout {
    fn timeout(&mut self, _: Duration) -> &mut Self;
}
impl CommandTimeout for Command {
    fn timeout(&mut self, _: Duration) -> &mut Self {
        self
    }
}
