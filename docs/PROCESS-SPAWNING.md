# Process spawning — design notes & deferred work

Reference audit: [Kobzol, "Process spawning performance in Rust" (2024)](https://kobzol.github.io/rust/2024/01/28/process-spawning-performance-in-rust.html).

This document captures *deliberate* deviations from the article's
recommendations, plus low-priority polish that's been deferred. The
audit was done in May 2026 against the agent-runner code in
`crabcc-cli`, `crabcc-viz`, and `apps/crabcc-agents`. Scorecard at the
time was 5 ✅ / 2 ⚠️ / 2 ❌.

## TL;DR

| # | Recommendation | Status | Rationale |
|---|----------------|--------|-----------|
| 1 | `kill_on_drop` on child handles | ✅ Fixed | `ChildGuard` RAII wrapper in `crabcc-cli/src/agent.rs` — see commit history. |
| 2 | `Command::env_clear()` + selective re-export | 🟡 Deferred (deliberate) | We *want* `PATH`/`TERM`/`SHELL` inheritance for agent ergonomics; see below. |
| 3 | `setsid()` for true session isolation in `spawn_detached` | 🟡 Deferred (low impact) | Agents rarely spawn descendants; current thread-poll reaper is sufficient. |

## #2 — env inheritance: deliberate

`crabcc-cli/src/agent.rs:344–367` builds the child env by inheriting
from the parent (`Command`'s default behaviour), then re-exporting a
short allowlist of auth vars (`HOME`, `XDG_*`, `ANTHROPIC_API_KEY`,
`OLLAMA_*`).

Kobzol shows that `env_clear()` followed by selective re-exports is
~15% faster than implicit inheritance, because the kernel doesn't have
to copy the full parent env into the child's address space.

**Why we don't do that:**

- Crabcc spawns one agent per CLI invocation (typically interactive,
  human-paced). The 15% saving is single-digit microseconds against a
  multi-second LLM round-trip — invisible.
- Inherited `PATH` lets the spawned `claude` find tools on the user's
  `$PATH` (e.g. `git`, `ripgrep`, project-local node_modules `.bin`)
  without a profile-author having to re-export every common var.
- Inherited `TERM`/`COLORTERM`/`SHELL` keeps the agent's own coloured
  output / prompt rendering aligned with the user's terminal, which
  the user has already configured.
- A future sandboxed runtime would use `env_clear()` — sandboxing is the
  right place for that level of control. The subprocess path is "you
  trust your shell".

**When to revisit:** if we ever spawn agents in tight loops (e.g. a
parallel-evaluation sweep, or a CI matrix that fans out across
hundreds of repos), measure first. The fix is one line.

## #3 — `setsid()` in `spawn_detached`

`crabcc-viz/src/lib.rs::spawn_detached` (around line 2191) launches
`crabcc agent --run` as a detached background process from the
`/api/agents/launch` HTTP handler. It uses a background thread that
polls `try_wait()` every 5 s, and on a hard timeout (20 min) calls
`child.kill()` + `child.wait()`.

Kobzol recommends running `setsid()` in a `Command::pre_exec` hook so
the child becomes its own process-group leader. Then on cleanup the
parent can `killpg()` the entire group, which catches any descendants
the agent might have started.

**Why we don't do that yet:**

- The agent runtime today is `claude` running in `--print` one-shot
  mode. It doesn't fork descendants under normal flows.
- The viz server's lifecycle is "long-running daemon"; the polled
  reaper handles the common timeout case.
- If the viz server crashes, the agent should keep running — that's
  the whole point of `spawn_detached` (the launch endpoint returns
  immediately; the agent's lifecycle is owned by its run-dir, not the
  HTTP request). `setsid()` is consistent with that model, but adding
  it isn't load-bearing today.

**When to revisit:** as soon as agents start spawning their own
subprocesses (think: an agent that runs `cargo build` or shells out
to `git`), and we observe orphaned subprocesses surviving an agent
timeout. At that point the right move is:

```rust
use std::os::unix::process::CommandExt;
unsafe {
    cmd.pre_exec(|| {
        libc::setsid();
        Ok(())
    });
}
// …on timeout:
unsafe { libc::killpg(child.id() as i32, libc::SIGTERM) };
```

## Where we're already aligned with the article

- ✅ `std::process::Command` routes to `posix_spawn(2)` on modern
  Linux/macOS — the fast path.
- ✅ Explicit `current_dir(req.root)` rather than inheriting the
  parent's cwd.
- ✅ `Stdio::piped()` + tee threads instead of blocking writes — log
  growth doesn't backpressure the agent.
- ✅ `apps/crabcc-agents` uses async `bollard` for Docker, not shell
  spawning. Container init has `--init`/`--read-only`/`cap-drop=ALL`
  and hard ulimits.
- ✅ Selective auth-var forwarding rather than blind blacklisting.
- ✅ Rust's stdlib auto-CLOEXEC-marks inherited FDs before exec, so
  we don't leak file descriptors into the child.

## Out of scope

- Pre-allocating `Command` / argv: only matters at >1k spawns/sec. We
  spawn at human cadence.
- Double-fork: `spawn_detached`'s thread-poll model is equivalent for
  our needs.
- High-frequency spawn batching: tracked under "if/when we ever
  parallelise the agent runner" — not on the roadmap.
