# `crabcc agent` runtime — design

`crabcc agent --run "<prompt>"` drives an LLM agent through one round of
tool-use against the crabcc MCP surface. This document covers:

1. The current runtime — host subprocess.
2. The trait seam that lets a future isolated runtime land without a rewrite.
3. The threat model we're explicitly making (and not yet making).

## Today: subprocess runtime

```
crabcc agent --run "find every public fn that doesn't have a unit test"
```

What happens, in order:

1. Resolve the repo root (`--root` global flag, otherwise cwd).
2. Best-effort `crabcc refresh` so the agent's first MCP call sees the
   current index (skip with `--no-refresh`).
3. Locate `claude` (or `claude-code`) on `PATH` — same lookup `crabcc go`
   uses today.
4. Read `AGENTS.md` (root → `.crabcc/AGENTS.md` fallback) into the
   `--append-system-prompt` slot. Falls back to a hardcoded primer.
5. Spawn:

   ```
   claude --print "<prompt>" \
          --append-system-prompt "<AGENTS.md body>" \
          [--model <id>] \
          --no-chrome
   ```

   `cwd` is the repo root, so the agent's tool calls (Bash, crabcc MCP)
   resolve against the right repo regardless of where `crabcc agent` was
   invoked.
6. Wait on the child; surface its exit code.

`--dry-run` short-circuits step 6 and prints the planned invocation
instead. The intent is to verify wiring (which AGENTS.md was picked,
which model, how many prompt chars) without burning tokens.

### Trust boundary today

The agent runs **as the invoking user, with no extra isolation** —
identical to running `claude` from a shell. Network, filesystem, and
process privileges are whatever the user already has. This is the same
trust posture as `crabcc go`.

This is **fine for opt-in single-user developer tooling**, where the
user is the one writing the prompt and reviewing the output. It is
**not fine** for:

- Untrusted prompts (e.g. piped from a webhook).
- Repos containing secrets the agent shouldn't touch.
- Workflows where the agent is expected to apply diffs without review.

A future sandboxed runtime would address those cases.

## Future: isolated runtime

The agent would get wrapped in a microVM or container. The sandbox mounts
only the repo root (read-write) plus the MCP socket — no host filesystem,
no host network, no host processes. Output is captured back over the same
socket / stdout pipe.

### Implementation seam

```rust
pub trait AgentRuntime {
    fn run(&self, request: &AgentRequest<'_>) -> Result<i32>;
    fn label(&self) -> &'static str;
}

pub struct SubprocessRuntime;   // current default
// A future SandboxRuntime impl drops in here; the trait is the contract.
```

The `AgentRuntime` trait decouples the dispatch logic from the backend.
Whoever lands the isolation work should be free to pick any backend
(Firecracker, Apple Virtualization.framework, runc+cgroups, etc.) without
touching the agent command infrastructure.

### Backend options

| Option | Isolation | Cold-start | Cross-platform | Maturity |
|---|---|---|---|---|
| **Firecracker direct** | microVM | ~125 ms | Linux KVM only | stable, AWS-grade |
| **Apple Virtualization.framework** | VM | ~500 ms | macOS only (10.15+) | stable, native |
| **runc + cgroups + seccomp** | container | <50 ms | Linux only | stable, `youki` Rust crate |
| **microsandbox** | microVM (libkrun) | "<100 ms" claimed | macOS arm64 + Linux KVM | not on crates.io as of 2026 |

## Trying it today

```bash
# 1. See exactly what would be invoked, no token spend:
crabcc agent --run "summarize the call-graph rooted at Store::open" --dry-run

# 2. Actually invoke (requires `claude` on PATH):
crabcc agent --run "summarize the call-graph rooted at Store::open"

# 3. Pin the model:
crabcc agent --run "..." --model claude-sonnet-4-6

# 4. Skip the implicit refresh when wrapping in a script:
crabcc agent --run "..." --no-refresh
```
