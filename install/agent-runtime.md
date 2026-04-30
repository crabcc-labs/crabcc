# `crabcc agent` runtime — design + v3.0 path

`crabcc agent --run "<prompt>"` drives an LLM agent through one round of
tool-use against the crabcc MCP surface. This document covers:

1. The current (v2.5.x) runtime — host subprocess.
2. The v3.0 plan (issue [#62](https://github.com/peterlodri-sec/crabcc/issues/62))
   — microVM-isolated runtime via [microsandbox](https://github.com/superradcompany/microsandbox)
   or an alternative.
3. The trait seam that lets v3.0 land as one cargo-feature flip, not a rewrite.
4. The threat model we're explicitly making (and not yet making).

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

The v3.0 runtime addresses those cases.

## v3.0: sandboxed runtime (issue #62)

The agent gets wrapped in a microVM. The microVM mounts only the repo
root (read-write) plus the MCP socket — no host filesystem, no host
network, no host processes. Output is captured back over the same
socket / stdout pipe.

### Implementation seam

```rust
pub trait AgentRuntime {
    fn run(&self, request: &AgentRequest<'_>) -> Result<i32>;
    fn label(&self) -> &'static str;
}

pub struct SubprocessRuntime;          // today; default
#[cfg(feature = "agent-sandbox")]
pub struct SandboxRuntime;             // v3.0; cargo-feature gated
```

The CLI dispatcher picks a runtime based on a future `--sandbox` flag
(coming with v3.0). Today the flag doesn't exist; `SubprocessRuntime`
is the only impl and is hardcoded in `agent::run`.

### Why a trait, not a config switch

Two reasons:

1. **The dep tree must not bloat for stable users.** microsandbox is
   itself a microVM dispatcher with a non-trivial dep tree (libkrun,
   OCI image management, hypervisor crates). Gating it behind a
   cargo feature keeps `cargo install crabcc` lean.
2. **v3.0 may not pick microsandbox.** Whoever lands #62 should be free
   to swap in `cloud-hypervisor`, direct Firecracker FFI, or even
   wasmtime if the analysis shifts. The `AgentRuntime` trait is the
   contract; the chosen backend is an implementation detail.

### Why microsandbox is the leading candidate (and the catch)

**Pro:** sub-second cold-start (claimed `<100ms` in the README, though
without per-platform numbers), libkrun-backed (smaller surface than
QEMU+KVM), Apache-2.0, OCI image model, simple "spin up and run a
command" API shape.

**Con as of 2026-04:**

- **Not on crates.io** (workspace version 0.4.2-beta) — git dep only.
- Self-declared **beta**.
- **Linux+KVM or macOS Apple Silicon only** — Windows + Intel Mac
  unsupported, even though the repo carries a `windows` topic tag.
- README claim of `<100ms` cold-start has no per-platform breakdown
  (a `benchmarks/` dir exists in the repo but the numbers aren't
  reproduced in user-facing docs).

The verdict for v3.0 planning: **design behind the trait now, defer the
backend decision until microsandbox publishes a stable crates.io
release** (or until a comparable alternative — Firecracker direct,
Apple's Virtualization.framework, runc + cgroups — looks better-fit).

### Comparable backends to keep on the design table

| Option | Isolation | Cold-start | Cross-platform | Maturity |
|---|---|---|---|---|
| **microsandbox** | microVM (libkrun) | "<100 ms" claimed | macOS arm64 + Linux KVM | beta, no crates.io |
| **Firecracker direct** | microVM | ~125 ms | Linux KVM only | stable, AWS-grade |
| **Apple Virtualization.framework** | VM | ~500 ms | macOS only (10.15+) | stable, native |
| **runc + cgroups + seccomp** | container | <50 ms | Linux only | stable, `youki` Rust crate |
| **wasmtime / Wasmer** | WASM sandbox | <10 ms | universal | stable, but agent CLIs aren't WASM |
| **macOS `sandbox-exec`** | profile-based | negligible | macOS only | stable, deprecated by Apple |

Wasmtime gets ruled out because `claude` (and any future agent CLI we'd
target) is not a WASM binary. Everything else is on the table.

## Acceptance for issue #62

| Criterion | Status |
|---|---|
| Spike: cold-start + one-shot `crabcc sym X` inside microsandbox | **deferred** — microsandbox not on crates.io as of 2026-04 |
| Decide: feature-gated dep vs companion binary | **decided** — feature-gated dep behind `agent-sandbox`; trait keeps the door open for a binary later |
| Document threat-model story | **this file** |
| Confirm cross-platform | **partial** — macOS arm64 + Linux KVM look possible; Windows + Intel Mac are gaps regardless of backend |

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
