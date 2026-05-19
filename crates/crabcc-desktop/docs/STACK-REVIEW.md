# crabcc Stack Review — Agents, MCP, and Container Plumbing

> Read-only architecture audit covering the whole `crabcc` workspace
> (Rust monorepo + supporting apps), focused on (1) the agent stack,
> (2) every MCP server in the codebase, and (3) what containerised
> agents actually see. Written 2026-05-05; based on the
> `feat/bootstrap-docker-default-on` worktree at HEAD `1ccac779`.

## 1. Executive summary

`crabcc` runs LLM agents on three nested orchestration tiers — host
subprocess, Apple `container compose` per-crate fan-out, and a
BullMQ-driven Docker worker pool — with a Caddy-gated LiteLLM proxy
in front of host Ollama feeding inference to all three. Every tier
ships with an MCP surface: `crabcc --mcp` (stdio/HTTP) on the host,
the desktop's planned in-process MCP host/server pair (call inspector
+ sampling-offer), an axint stdio server baked into the agent-runner
image, and a FastMCP `chat` tool exposed by the HITL Python service.
The recursion goes Claude Code → `crabcc agent run` (subprocess) →
optional BullMQ enqueue → distroless agent-runner container → in-
container `claude code` → MCP back to axint (stdio) and (planned) to
the host desktop for sampling. **Path / cwd propagation is solid for
SubprocessRuntime but lossy for BullmqRuntime** — `req.root` is never
serialised onto the BullMQ job (`apps/crabcc-agents/src/runner.rs` mounts
only a tmpfs `/workspace`), so containerised agents see an empty repo.
The other observed gap is that the `BullmqRuntime` host-side never
starts the Ollama stack (only `SubprocessRuntime` calls
`ollama_stack::ensure_up`), so a queued job hits cold LiteLLM.

## 2. Layer map (top-down)

```
╔═══════════════════════════════════════════════════════════════════════╗
║ L0  USER / OPERATOR                                                   ║
║     Click in desktop · type in Claude Code · send Telegram message    ║
╚═══════════════════════════════════════════════════════════════════════╝
                  │                          │
                  ▼                          ▼
┌────────────────────────────┐   ┌───────────────────────────────────┐
│ L1a  Claude Code (host)    │   │ L1b  crabcc-desktop (gpui binary) │
│   - parent process         │   │   - in-proc MCP host (planned)    │
│   - reads ~/.claude/*      │   │   - SamplingHandler (just landed) │
│   - speaks MCP stdio       │   │   - bootstraps docker stack       │
│     to crabcc, fff, …      │   │     services.rs:105               │
└──────────┬─────────────────┘   └────────────┬──────────────────────┘
           │ spawns                           │ spawns / probes
           ▼                                  ▼
┌────────────────────────────────────────────────────────────────────┐
│ L2  crabcc CLI host process                                        │
│   crates/crabcc-cli/src/agent.rs:657  pub fn run(req)             │
│   - resolves req.root, opens RunDir under ~/.crabcc/agents/<id>/  │
│   - writes meta.json + lock + log + pid                           │
│   - dispatches to one of three AgentRuntime impls (agent.rs:783)  │
│        ├── SubprocessRuntime       (default)                      │
│        ├── SandboxRuntime          (#cfg agent-sandbox; stub)     │
│        └── BullmqRuntime           (#cfg agents-bullmq)           │
└──────────┬──────────────────────────────────────┬──────────────────┘
           │ subprocess fork+exec                 │ redis enqueue
           ▼                                      ▼
┌──────────────────────────┐         ┌────────────────────────────────┐
│ L3a  Host claude CLI     │         │ L3b  crabcc-agents worker      │
│   /usr/local/bin/claude  │         │   apps/crabcc-agents (Rust)    │
│   - inherits HOME, PATH, │         │   - bollard Docker client      │
│     ANTHROPIC_API_KEY    │         │   - connects /var/run/docker   │
│   - --mcp-config picks   │         │     (bind-mounted into worker) │
│     up host configs      │         │   - Redis Streams for stdout   │
│   - cwd = req.root       │         │   - itself in a Docker         │
│                          │         │     container (compose service)│
└────────┬─────────────────┘         └────────┬───────────────────────┘
         │ stdio MCP                          │ docker.create_container
         │                                    ▼
         │                      ┌────────────────────────────────────┐
         │                      │ L4  agent-runner container         │
         │                      │   ghcr.io/.../crabcc-agent-runner  │
         │                      │   apps/crabcc-agents/agent-runner/ │
         │                      │   - tini PID1 + /workspace tmpfs   │
         │                      │   - Alpine + node22 + claude CLI   │
         │                      │   - cap_drop=ALL, readonly_rootfs  │
         │                      │   - network=none unless overridden │
         │                      │   - bind-mount ~/.claude/cred RO   │
         │                      │   - claude code -p "$prompt"       │
         │                      └─────┬──────────────────────────────┘
         │                            │ HTTP (when AXINT_MCP_URL set)
         │                            │ or stdio in-container
         ▼                            ▼
┌──────────────────────────────────────────────────────────────────────┐
│ L5  MCP servers (multiple, see §4)                                   │
│   - crabcc-mcp     stdio + HTTP   crates/crabcc-mcp/src/lib.rs:42    │
│   - axint          stdio (in-container) or HTTP (host sidecar)       │
│   - hitl-agent     streamable-http  apps/.../mcp_server.py:36        │
│   - desktop self   in-proc (PLANNED, MCP-NATIVE.md §4.1)             │
└─────────────────────────┬────────────────────────────────────────────┘
                          │ outbound LLM via reqwest::blocking
                          ▼
┌──────────────────────────────────────────────────────────────────────┐
│ L6  LiteLLM proxy (Docker)                                           │
│   install/ollama-stack/docker-compose.yml:149                        │
│   - ghcr.io/berriai/litellm:main-stable                              │
│   - host port :4000 (loopback only)                                  │
│   - Bearer LITELLM_MASTER_KEY                                        │
│   - OpenAI-shaped /v1/chat/completions                               │
│   - hops to caddy:11434 with OLLAMA_API_KEY                          │
└─────────────────────────┬────────────────────────────────────────────┘
                          │ HTTP+Bearer
                          ▼
┌──────────────────────────────────────────────────────────────────────┐
│ L7  Caddy auth proxy (Docker)                                        │
│   install/ollama-stack/docker-compose.yml:108                        │
│   - caddy:2-alpine, readonly rootfs, cap_drop=ALL                    │
│   - host port :11435 (loopback only)                                 │
│   - enforces Authorization: Bearer ${OLLAMA_API_KEY}                 │
└─────────────────────────┬────────────────────────────────────────────┘
                          │ HTTP (intra-stack)
                          ▼
┌──────────────────────────────────────────────────────────────────────┐
│ L8  Ollama (Docker)                                                  │
│   install/ollama-stack/docker-compose.yml:40                         │
│   - ollama/ollama:latest, expose 11434 only (no host port)           │
│   - volumes: ollama_models, $HOME/.crabcc:/home/crabcc/.crabcc:ro    │
│   - OLLAMA_NUM_PARALLEL=4, OLLAMA_NUM_CTX=65536                      │
│   - on Apple Silicon: Metal backend on host GPU                      │
└──────────────────────────────────────────────────────────────────────┘
```

What each layer can see:

| Layer | Filesystem | Network | Env it inherits |
|---|---|---|---|
| L1 Claude Code | full host fs as user | unrestricted | shell env |
| L2 crabcc CLI | full host fs | unrestricted | shell env + `CRABCC_*` |
| L3a host claude | full host fs | unrestricted | inherited + `OLLAMA_BASE_URL`/`OLLAMA_API_KEY` (`agent.rs:360`) + biased `PATH` (`agent.rs:381`) |
| L3b agents worker | only `/var/run/docker.sock` from host (`docker-compose.yml:52`) | docker network only | per `apps/crabcc-agents/.env.example` |
| L4 agent-runner | tmpfs `/workspace` (1 GiB), tmpfs `/tmp` (512 MiB), readonly rootfs, **no host bind**, RO mount of `~/.claude/.credentials.json` only (`runner.rs:255`) | `none` (default) or `bridge` if `HOST_AXINT_MCP_URL` set (`runner.rs:222`) | env composed in `runner.rs:286` `compose_env` |
| L5 MCP servers | host fs (in-proc / stdio child) | depends on transport | inherited |
| L6 LiteLLM | tmpfs `/tmp` only, RO config | docker network | `LITELLM_MASTER_KEY`, `OLLAMA_API_BASE`, `OLLAMA_API_KEY` |
| L7 Caddy | RO rootfs | docker network | `OLLAMA_API_KEY` |
| L8 Ollama | volume `ollama_models`, `$HOME/.crabcc:ro` | docker network | OLLAMA_* tuning vars |

## 3. Agents inside agents

The recursion stack, with the file:line that materialises each step:

```
┌────────────────────────────────────────────────────────────────────┐
│ Tier 0 — user / Claude Code                                        │
│ Claude Code is itself an agent; it sees the crabcc skill at        │
│ ~/.claude/skills/crabcc/ (symlinked by `crabcc install-claude`)    │
│ and configures `crabcc --mcp` as an MCP server                     │
│ (CLAUDE.md "Slash commands & skill" §).                            │
└─────────────────────────────┬──────────────────────────────────────┘
                              │  user runs `/crabcc-agent <prompt>`
                              │  or `crabcc agent --run "..."`
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│ Tier 1 — `crabcc agent` host process                               │
│ crates/crabcc-cli/src/agent.rs:657  pub fn run(req)                │
│  - RunDir under ~/.crabcc/agents/<id>/      (agent.rs:175)         │
│  - ollama_stack::ensure_up if Backend::Ollama (agent.rs:764)       │
│  - dispatches AgentTransport::{Subprocess,Bullmq} (agent.rs:783)   │
└──────┬───────────────────────────────────┬─────────────────────────┘
       │                                   │
   Subprocess                          BullMQ
       │                                   │
       ▼                                   ▼
┌──────────────────────┐    ┌──────────────────────────────────────┐
│ Tier 2a — host       │    │ Tier 2b — bullmq_rs enqueue          │
│   claude CLI         │    │   crates/crabcc-cli/src/agent_bullmq │
│   agent.rs:317       │    │   .rs:131 RedisConnection::new       │
│   - cwd = req.root   │    │   - QueueBuilder + AgentJob          │
│   - --mcp-config     │    │   - host writes job; tails stream    │
│     resolved by      │    │     crabcc:agent:logs:<job_id>       │
│     claude itself    │    │     (agent_bullmq.rs:175)            │
│     from ~/.claude/  │    │                                      │
└─────┬────────────────┘    └────────────────┬─────────────────────┘
      │                                       │
      │ stdio MCP                            redis BLPOP
      ▼                                       ▼
┌──────────────────────┐    ┌──────────────────────────────────────┐
│ Tier 3a — child MCP  │    │ Tier 3 — crabcc-agents worker        │
│   servers spawned    │    │   apps/crabcc-agents/src/main.rs     │
│   by Claude Code:    │    │   - Runner::run                      │
│   - crabcc           │    │     apps/crabcc-agents/src/runner.rs │
│   - fff              │    │     :96                              │
│   - context-mode     │    │   - bollard create_container         │
│   - axint (host)     │    │     (runner.rs:147)                  │
│   etc.               │    └────────────────────┬─────────────────┘
│                      │                         │
│   (tier-3 servers    │                         │ docker.start
│    are themselves    │                         ▼
│    OS subprocesses;  │    ┌──────────────────────────────────────┐
│    same trust as     │    │ Tier 4 — agent-runner container      │
│    host claude)      │    │   image: crabcc-agent-runner:latest  │
└──────────────────────┘    │   ENTRYPOINT tini → entrypoint.sh    │
                            │     apps/.../agent-runner/Dockerfile │
                            │     :118 + entrypoint.sh:118         │
                            │   - dispatches AGENT_KIND:           │
                            │     · claude-code (default) (147)    │
                            │     · mini-swe-agent (123)           │
                            │   - claude code --sandbox            │
                            │     --mcp-config /etc/.../mcp.json   │
                            │     (entrypoint.sh:99 + :92)         │
                            └────────────────────┬─────────────────┘
                                                 │
                                                 │ stdio MCP
                                                 ▼
                            ┌──────────────────────────────────────┐
                            │ Tier 5 — axint MCP (in-container)    │
                            │   apps/.../agent-runner/mcp.json:3   │
                            │   command="axint" args=["mcp"]       │
                            │                                      │
                            │   OR (host mode):                    │
                            │   AXINT_MCP_URL=http://host.docker   │
                            │   .internal:7785/mcp                 │
                            │   (entrypoint.sh:81 rewrites mcp.json│
                            │    in /tmp tmpfs)                    │
                            └──────────────────────────────────────┘
```

**Can a Tier-4 container spawn more agents?** In principle yes —
`claude code` running inside the runner has the Bash tool available
unless `CLAUDE_DISABLE_BASH=1` (set when `payload.sandbox.bash=false`,
`runner.rs:313`). Therefore `claude` *could* call `crabcc agent --run`
recursively if the `crabcc` binary were installed in the image. **It
is not** (Dockerfile shows only `claude-code`, `axint`, `mini-swe-agent`,
`rtk`, `context-mode`; `crabcc` itself is absent). So in practice
tier-4 cannot recurse via `crabcc agent`. It *can* recurse via
`claude code` re-invocation if the agent literally types `claude code`
into Bash, but the `--mcp-config` is baked at /etc/crabcc-agent/mcp.json
and only exposes `axint`, so a recursive call would have a thinner MCP
surface than the parent.

The MCP-NATIVE.md design contemplates a **Tier 5 → Tier 1' loop**: the
container talks MCP back to the host desktop (Unix socket bind-mount or
VSOCK) and asks for `sampling/createMessage`. The desktop fulfils the
request via LiteLLM (§7.2 below). This is the planned path; the socket
bind-mount is **not yet present in `runner.rs::host_config`** — only
the credentials file mount is wired today (`runner.rs:255`).

A separate fan-out path exists for **per-crate "internal agents"**
(`install/internal-agents/compose.yml`). Five Apple-`container`-compose
services (`crabcc-core`, `crabcc-cli`, `crabcc-mcp`, `crabcc-memory`,
`crabcc-viz`) each launch one agent specialised in one crate, sharing
a `cargo-cache` volume and bind-mounting the repo root R/W
(`compose.yml:23` — `..:/workspace`). This path is independent of
BullMQ; it's the "five experts ganging up on the workspace" model.

## 4. MCP server inventory

| # | Name | Location | Transport | Status | Tools / resources / prompts | Consumed by |
|---|---|---|---|---|---|---|
| 1 | `crabcc-mcp` (stdio) | `crates/crabcc-mcp/src/lib.rs:42 serve_stdio` | newline-delim JSON-RPC over stdio | Implemented | `sym`, `refs`, `callers`, `outline`, `index`, `refresh`, `_openapi`, plus `memory.*` (10 tools) per `crabcc-mcp/src/memory.rs` | Claude Code via `claude mcp add crabcc -- crabcc --mcp`; Cursor; any host installing `crabcc install-claude` |
| 2 | `crabcc-mcp` (HTTP) | `crates/crabcc-mcp/src/lib.rs:150 serve_http` | tiny_http JSON-RPC POST `/mcp` + `GET /health`, optional Bearer | Implemented (#204 phase 1 — sync only; SSE pending phase 4) | same as (1) | Network-attached agents, telegram bot, browser bridge |
| 3 | `axint` MCP (in-container) | `apps/crabcc-agents/agent-runner/mcp.json:3` + `Dockerfile:57` (`@axint/compiler@${AXINT_VERSION}`) | stdio child of `claude code` | Implemented | per axint upstream: `suggest`, `workflow`, `repair`, et al. (see global MEMORY.md: TS→Apple App Intents compiler) | The agent-runner container's `claude code` instance |
| 4 | `axint-mcp-http` (host sidecar) | `apps/crabcc-agents/docker-compose.yml:66` (commented sidecar) + `runner.rs:226` extra_hosts | HTTP `:7785/mcp` | Optional / opt-in (commented out by default) | same as (3) but shared across containers | Containers when `HOST_AXINT_MCP_URL` is set |
| 5 | `crabcc-hitl-agent` | `apps/crabcc-hitl-agent/src/crabcc_hitl/mcp_server.py:36 build_mcp` | streamable-HTTP `/mcp`, port `CRABCC_HITL_MCP_PORT` (default `9101`) | Implemented | one tool: `chat(task)` (`mcp_server.py:54`) | Other host services; future Telegram bot path; "Rust crabcc-mcp consumers, future agents in the workspace" per the docstring |
| 6 | Desktop self-server (call inspector + sampling) | `crates/crabcc-desktop/docs/MCP-NATIVE.md` §3 + `crates/crabcc-desktop/src/sampling.rs` | Planned: in-proc + Unix socket / VSOCK + Tailscale (iPhone). M0 ring buffer is in-proc; external transport is M2+ in the roadmap. | **Sampling handler implemented** (`sampling.rs:466 LiteLlmSamplingHandler::handle`); inspector ring observed via `InspectorSamplingObserver` (`inspector.rs:344`). External MCP transport is **design-only** in `MCP-NATIVE.md` §9 M2/M3. | Planned tools: `desktop.route.show`, `desktop.agent.{spawn,kill}`, `desktop.notify`, `desktop.memory.*`, `desktop.command.run`, `desktop.window.*` (`MCP-NATIVE.md` §3.1). Resources: `desktop://routes/current`, `desktop://agents/{id}`, `desktop://logs/{id}`, `desktop://mcp/calls`, `desktop://mcp/servers`. **Sampling capability advertised outward** (`MCP-NATIVE.md` §3.4 + `MCP-SAMPLING-OFFER.md`). | Future: Claude Code consuming desktop as peer; iPhone client; BullMQ container agents (credential-free inference) |
| 7 | `context-mode` MCP | `agent-runner/Dockerfile:63` (`npm install -g context-mode \|\| ...`) + `entrypoint.sh:23` | stdio (managed by claude-code via PreToolUse hook + MCP registration) | Best-effort install (Dockerfile tolerates missing package) | per upstream context-mode | The agent-runner container when `CRABCC_CONTEXT_MODE=1` |
| 8 | `fff` MCP | Referenced in `MCP-NATIVE.md:202` example registry block | stdio child (planned) | Design-only example | per fff upstream | Desktop registry (planned M1 of MCP-NATIVE.md §9) |

Per the workspace note in `Cargo.toml:18`, `crates/crabcc-desktop` is
**workspace-excluded**, which means none of (6)'s MCP code is built in
default `cargo build --workspace` — it ships only when the user does
`cd crates/crabcc-desktop && cargo run --release`.

## 5. Container context plumbing — `BullmqRuntime`

This is the section the user specifically asked me to verify. Here's
what gets passed in, and what's missing.

### 5.1 Producer side (host CLI → Redis)

`crates/crabcc-cli/src/agent_bullmq.rs:149` builds `AgentJob`:

```
AgentJob {
    prompt:    req.prompt,
    kind:      AgentKind::ClaudeCode | MiniSwe (env CRABCC_AGENT_KIND),
    model:     req.model,
    effort:    env AGENT_DEFAULT_EFFORT,
    sandbox:   SandboxSpec::default()  // network=false, writeable=false, bash=true
    env:       HashMap::new(),         // EMPTY — no per-job env from CLI
    timeout_secs: None,
    headers:   { x-source=cli, x-job-run-id=run.id, plus CRABCC_HEADER_* }
}
```

**`req.root` is never serialised onto the job** (`agent_bullmq.rs:149-163`).
The container has no idea which host repo it was launched from.

### 5.2 Consumer side (worker → container)

`apps/crabcc-agents/src/runner.rs::host_config` builds the
`bollard::HostConfig`:

| Mount / setting | Value | Source |
|---|---|---|
| `working_dir` | `/workspace` (hard-coded) | `runner.rs:143` |
| `/workspace` mount | tmpfs, 1 GiB by default, `nodev,nosuid` | `runner.rs:233` (`agent_tmpfs_workspace_bytes`) |
| `/tmp` mount | tmpfs, 512 MiB | `runner.rs:241` |
| `~/.claude/.credentials.json` | bind-mount RO into `/home/nonroot/.claude/.credentials.json` | `runner.rs:255` |
| Host repo root | **not mounted** | — |
| Host `.crabcc/` | **not mounted** | — |
| Memory.db | **not mounted** | — |
| MCP socket from host | **not mounted** | — |
| `readonly_rootfs` | `true` unless `payload.sandbox.writeable_root` | `runner.rs:268` |
| `cap_drop` | `ALL` | `runner.rs:270` |
| `security_opt` | `no-new-privileges` | `runner.rs:271` |
| `network_mode` | `none` unless `payload.sandbox.network` OR `host_axint_mcp_url` is set, in which case `agent_network` (default `bridge`) | `runner.rs:222` |
| `extra_hosts` | `host.docker.internal:host-gateway` only when `host_axint_mcp_url` set | `runner.rs:226` |
| `pids_limit` | 256 | `runner.rs:277` |
| `memory` | 2 GiB default | `config.rs:126` |
| `ipc_mode` | `private` | `runner.rs:279` |

### 5.3 Env vars passed to the container

`runner.rs::compose_env` (`runner.rs:286`):

```
PROMPT=<payload.prompt>
AGENT_KIND=<claude-code|mini-swe>
RUST_LOG=info
CI=1
CLAUDE_NONINTERACTIVE=1
CRABCC_RTK=1
CRABCC_CONTEXT_MODE=1
CLAUDE_MODEL=<payload.model || cfg.default_model>
CLAUDE_EFFORT=<payload.effort || cfg.default_effort>
[CLAUDE_DISABLE_BASH=1]                     # if !sandbox.bash
[AXINT_MCP_URL=<cfg.host_axint_mcp_url>]    # opt-in
[CLAUDE_CODE_OAUTH_TOKEN=<cfg.claude_oauth_token>]
                                            # only when no creds file mounted
[<payload.env...>]                          # user-supplied; CLI sets to {}
[CRABCC_HEADER_X_REQUEST_ID=...]            # one entry per header
```

**Missing from the container env:**

- `OLLAMA_BASE_URL` — host CLI propagates this for SubprocessRuntime
  (`agent.rs:360`) but the worker never forwards it; the agent inside
  the container doesn't know which LiteLLM to talk to.
- `LITELLM_BASE_URL` / `LITELLM_MASTER_KEY` — never set inside the
  container. The agent's `claude code` calls Anthropic directly using
  the SSO credentials file. `mini-swe-agent` does the same.
- Host repo path — there is no `CRABCC_REPO_ROOT`, no `WORKSPACE_HOST`,
  no `PROJECT_PATH` env. The agent has no way to know which directory
  it was launched against.
- `CRABCC_HOME` — the host's `~/.crabcc/` is invisible. Memory.db,
  index.db, agent run dirs (`~/.crabcc/agents/<id>/`) cannot be read or
  written from inside.

### 5.4 Cross-cutting: the worker container vs the runner container

Note the **two-layer Docker** here. The BullMQ worker (`apps/crabcc-agents`)
is itself shipped as a Docker container in `apps/crabcc-agents/docker-compose.yml`,
which bind-mounts `/var/run/docker.sock` so it can spawn peer
containers (`docker-compose.yml:52`). When the user runs
`crabcc-agents` natively (no compose, just `cargo run`), it uses
`Docker::connect_with_local_defaults()` (`runner.rs:34`) which finds
the daemon via the standard env / socket.

## 6. Capability matrix

Layers vs capabilities. ✓ = available, ✗ = not available, P = planned.

| Layer | read host fs | write host fs | run shell | network | GPU | hit Ollama | hit LiteLLM | talk MCP | read memory.db | spawn agents |
|---|---|---|---|---|---|---|---|---|---|---|
| Host shell | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ via :11434 | ✓ via :4000 | ✓ | ✓ | ✓ |
| Desktop process (gpui) | ✓ | ✓ | ✓ (via `services.rs:133` `docker compose`) | ✓ | indirect (Metal via gpui) | ✓ | ✓ (`sampling.rs:31`) | ✓ in-proc (planned external in MCP-NATIVE.md) | ✓ via `Client` | ✓ via `submit_command_run` (`state.rs:1019`) |
| `crabcc agent` host process | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ recursively |
| SubprocessRuntime child (host claude) | ✓ as user | ✓ as user | ✓ Bash tool | ✓ | ✓ | ✓ | ✓ via `OLLAMA_BASE_URL` env | ✓ via host stdio MCP servers | ✓ via crabcc-mcp | ✓ if `crabcc agent` on PATH |
| BullmqRuntime worker (in worker container) | only via mounted socket | ✗ | ✗ (no shell exec'd; just Docker API) | docker network | indirect | not directly | not directly | ✗ | ✗ | ✓ — that's its job |
| agent-runner container (Tier 4) | ✗ (only tmpfs `/workspace` + RO creds file) | ✗ rootfs RO; ✓ `/workspace` tmpfs only | ✓ unless `CLAUDE_DISABLE_BASH=1` | ✗ default `none`; ✓ when `network=true` or `host_axint` set | ✗ no GPU passthrough | ✗ — no `OLLAMA_BASE_URL` set | ✗ — no `LITELLM_*` env | ✓ stdio to axint; HTTP to host axint when set | ✗ no mount | P (no `crabcc` binary in image) |
| Apple `container` internal-agents (Tier 1') | ✓ repo bind-mounted RW (`compose.yml:23`) | ✓ to repo | ✓ | ✓ host bridge | ✗ no Apple GPU passthrough | host gateway via host.docker.internal | host gateway | ✓ stdio child | indirectly (repo bind) | ✓ via `crabcc agent` baked in image |
| LiteLLM container | ✗ (RO config only) | tmpfs `/tmp` only | ✗ no shell needed | docker network | ✗ | yes — talks to caddy | itself | ✗ | ✗ | ✗ |
| Caddy container | ✗ (RO Caddyfile) | tmpfs `/tmp` only | ✗ | docker network | ✗ | yes (intra-stack) | n/a | ✗ | ✗ | ✗ |
| Ollama container | RO `$HOME/.crabcc` (compose.yml:61) + `ollama_models` volume RW | volume only | ✗ | docker network | ✓ via Metal on host (Apple Silicon) | itself | ✗ | ✗ | ✗ | ✗ |

## 7. Use-case walkthroughs

### 7.1 Claude Code asks crabcc to summarise the repo

1. User types in Claude Code: "summarise the call graph rooted at `Store::open`".
2. Claude Code consults its MCP server registry (host `~/.claude/`,
   wired by `crabcc install-claude`).
3. It dispatches `tools/call` for `crabcc.callers` over **stdio JSON-RPC**
   to the `crabcc --mcp` subprocess.
4. The subprocess routes through `serve_io` → `handle_with` (`crates/crabcc-mcp/src/lib.rs:85` and `:116`).
5. Inside `handle_with`, the call hits `crabcc_core::query` against
   `.crabcc/index.db` (resolved via the `root` parameter the MCP
   `initialize` handshake recorded).
6. JSON result is returned as a single text content block
   (`lib.rs:7-10` docstring) and Claude Code streams it back into the
   chat. **No LLM inference is involved on the crabcc side.** Inference
   is whatever Claude Code is configured to use.

If the user instead types `crabcc agent --run "summarise …"`:

1. `crabcc-cli/src/agent.rs:657 run` opens a `RunDir`.
2. `Backend::Ollama` (default since v2.8) triggers
   `crabcc_core::ollama_stack::ensure_up` (`agent.rs:772`) — this brings
   up Ollama+Caddy+LiteLLM.
3. `SubprocessRuntime` spawns `claude` with `cwd=req.root` and env
   `OLLAMA_BASE_URL`/`OLLAMA_API_KEY` (`agent.rs:360`).
4. `claude code` does its own MCP discovery from `~/.claude/`, hits
   `crabcc --mcp` AND any other MCPs the user has installed (e.g.
   slack, fff, axint).
5. When `claude code` chooses to think, it calls `OLLAMA_BASE_URL`
   (which is the LiteLLM proxy, despite the env name) and that path
   loops down through Caddy → Ollama.
6. Output is teed into `~/.crabcc/agents/<id>/log` (`agent.rs:433`).

### 7.2 Desktop sampling-offer smoke test (`Commands → MCP → sampling.test`)

The freshly-landed path:

1. User clicks "MCP → sampling.test" in the Commands route.
2. `commands.rs:80` resolves to `RunnableCommand::TestSampling`.
3. `state.rs:1252` dispatches to `run_test_sampling(tx)`.
4. `run_test_sampling` (`state.rs:1261`) calls
   `LiteLlmSamplingHandler::from_env()` which reads `LITELLM_MASTER_KEY`
   (`sampling.rs:401`) and builds the handler.
5. An `InspectorSamplingObserver` is attached (`state.rs:1270`,
   `inspector.rs:344`) — every request/response will land in the
   inspector ring buffer as a `CallEvent`.
6. The handler builds a `SamplingRequest` with hints `["qwen3.5", "qwen2.5-coder"]` and `costPriority=1.0` (`state.rs:1283`).
7. `SamplingHandler::handle` (`sampling.rs:467`) gates on
   `samplingDepth >= MAX_SAMPLING_DEPTH(=3)`, runs `select_model`, and
   calls `do_call`.
8. `do_call` POSTs to `DEFAULT_LITELLM_ENDPOINT = "http://127.0.0.1:4000/v1/chat/completions"` (`sampling.rs:31`) using
   `reqwest::blocking` with `Authorization: Bearer ${LITELLM_MASTER_KEY}`.
9. LiteLLM container routes the OpenAI-shaped request to Ollama via
   the Caddy-gated proxy (compose.yml:169 `OLLAMA_API_BASE: http://caddy:11434`).
10. Response flows back; `InspectorSamplingObserver::on_response` writes
    the latency + token counts onto the inspector ring; the JSON body
    is rendered in the Commands route.

This is **the M3 demo path** described in `MCP-NATIVE.md` §9 / §3.4.
The external-server side of the loop (a containerised agent calling
`sampling/createMessage` *back into* the desktop) is not yet built —
no Unix-socket bind-mount in `runner.rs::host_config`, no MCP
listener in the desktop process, no peer-identity store at
`~/.crabcc/desktop/peers.toml` (referenced in `MCP-CONSENT.md:49`).

### 7.3 BullMQ-spawned agent runs a task that needs Ollama

1. User runs `crabcc agent --transport bullmq --run "<task>"`.
   `AgentTransport::Bullmq` is gated behind the `agents-bullmq` Cargo
   feature (`agent.rs:101`). Without that build flag, the user gets a
   helpful error pointing them at the feature flag (`agent.rs:122`).
2. **Crucially**: the host-side `ollama_stack::ensure_up` only fires
   for `SubprocessRuntime` (`agent.rs:764` is bracketed by `req.backend == Backend::Ollama` AND `req.transport == Subprocess` only by virtue
   of running before the runtime dispatch — actually it runs **before
   the dispatch**, so it does fire even for Bullmq. But the LiteLLM
   stack the host brings up is on the host network namespace, and the
   BullMQ container has `network=none` by default — see step 6).
3. `BullmqRuntime::run` (`agent_bullmq.rs:88`) opens a tokio
   single-thread runtime and calls `run_async`.
4. `run_async` connects to Redis (default `redis://127.0.0.1:6379`),
   builds an `AgentJob` (no `req.root`! see §5.1), enqueues onto
   `crabcc:agents`.
5. The standalone `crabcc-agents` worker BLPOPs the queue
   (`apps/crabcc-agents/src/main.rs`), routes to `Runner::run`
   (`runner.rs:96`).
6. `Runner::host_config` builds the `bollard::HostConfig` with
   `network_mode=none` unless `payload.sandbox.network=true` OR
   `host_axint_mcp_url` is set (`runner.rs:222`). **Default is
   `network=none`.** With network=none, the container cannot reach
   `host.docker.internal:4000` even if it had `LITELLM_BASE_URL` set.
7. `Runner::compose_env` produces env for the container (`runner.rs:286`).
   `OLLAMA_BASE_URL` is **not** in this env. `LITELLM_BASE_URL` is **not**
   in this env. The agent inside has no way to reach the host LiteLLM
   stack.
8. The container starts with `claude code -p "<prompt>"`. Its only LLM
   credential is the bind-mounted SSO file at
   `/home/nonroot/.claude/.credentials.json` (`runner.rs:255`). Claude
   Code uses that to talk to **Anthropic directly** — **not** to the
   local Ollama stack.

**Summary**: the BullMQ path today is configured to use cloud
Anthropic via SSO, not local Ollama. The text "BullMQ-spawned agent
runs a task that needs Ollama" doesn't actually have a wired path
end-to-end; it's the design ambition of `MCP-NATIVE.md` §3.4
("agents launched via `BullmqRuntime` never carry their own model
credentials. They speak MCP back to the desktop … and request
sampling.") — but the bridge is still on paper.

## 8. Gaps / risks

Specifically about path/cwd/env propagation, since the user flagged
that:

1. **`req.root` is dropped at the BullMQ enqueue boundary**
   (`agent_bullmq.rs:149`). The worker has no idea which host repo
   the agent should target. The container's `working_dir` is the
   hard-coded `/workspace` tmpfs (`runner.rs:143`), with **no host
   bind**. This is the user's "make sure paths (folders etc) proper
   context IS available inside the containers" concern, and it is
   currently **NOT satisfied** for BullMQ. SubprocessRuntime is fine
   (`cmd.current_dir(req.root)` — `agent.rs:342`).

2. **Worker container does not forward `OLLAMA_BASE_URL` /
   `LITELLM_BASE_URL` into the agent container.** `compose_env`
   (`runner.rs:286-347`) lists every env it sets, and these are not in
   the list. So even if the network problem (gap #6) is fixed, the
   in-container agent can't dial the host stack. The host CLI
   *does* propagate these to subprocesses (`agent.rs:360`); only the
   queue-spawned path is broken.

3. **Worker container does not bind-mount the host's `~/.crabcc/`.**
   The Ollama compose service mounts `$HOME/.crabcc:/home/crabcc/.crabcc:ro`
   (`install/ollama-stack/docker-compose.yml:61`) so Ollama can read
   the host's index for prompts. The agent-runner does not. Therefore
   `claude code` running inside the container cannot consult the same
   `memory.db` / `index.db` / `graph.db` as the host invocation —
   it's a fresh, contextless agent.

4. **No MCP socket bind-mount.** `MCP-NATIVE.md` §4.4 promises
   "Reached over Unix socket bind-mount or VSOCK; the socket itself is
   the capability". `runner.rs::host_config` has no such mount today —
   the only mount is the SSO credentials file. The container can only
   reach axint MCP (in-container or via `host.docker.internal:7785/mcp`),
   never crabcc-mcp / desktop-self-mcp / hitl-agent-mcp.

5. **Apple `container` plumbing is not wired into `BullmqRuntime`.**
   `install/internal-agents/compose.yml` is laid out for Apple
   `container compose`, but the BullMQ runner uses `bollard` against
   the Docker daemon socket (`runner.rs:34`). The two paths are
   independent — there's no Apple-`container` AgentRuntime impl;
   `SandboxRuntime` (`agent.rs:523`) is a stub that returns `Ok(0)` on
   `--dry-run` and is otherwise unimplemented.

6. **`network=none` plus host axint detection couples two concerns.**
   `runner.rs:221` flips the network from `none` to `bridge` when
   `host_axint_mcp_url` is set. This is the *only* way the container
   gets network. There's no opt-in for "I want to reach the LiteLLM
   stack but not necessarily axint". A future agent that wants
   sampling but not axint MCP has to flip `payload.sandbox.network` on
   manually, which then opens *all* network egress. Finer-grained
   network policy isn't there yet.

7. **`BullmqRuntime::run` doesn't honour `req.no_refresh` or
   `req.dry_run`-with-side-effects** the same way Subprocess does
   (`agent.rs:733` / `agent.rs:384`). Dry-run is honoured
   (`agent_bullmq.rs:89`) but the index pre-warm (`agent.rs:734-758`)
   has already run *before* the transport dispatch — fine for both
   paths, but only the host index is touched. The container gets a
   fresh tmpfs `/workspace` regardless.

8. **No serialisable agent identity flowing into the container.**
   `set_active_agent_id` (`agent.rs:666`) stamps a process-local
   thread-local for activity tracking. This stays on the host. The
   container has no way to know its own `agent_id` — `headers` carry
   `x-job-run-id` (`agent_bullmq.rs:147`) and the runner translates
   them to `CRABCC_HEADER_X_JOB_RUN_ID` (`runner.rs:343`), so it *is*
   recoverable, but it's not exposed under the canonical
   `CRABCC_AGENT_ID` name that `crabcc-core::track` would look for.

9. **The desktop's "always start docker stack" bootstrap
   (`services.rs:133`) is an unconditional `docker compose -f .../install/dev/docker-compose.yml up -d`.** This is a different
   compose file from the BullMQ path's
   `apps/crabcc-agents/docker-compose.yml`, AND a different file from
   the Ollama auth stack at `install/ollama-stack/docker-compose.yml`.
   Three independent compose graphs, each side-effected separately,
   and the desktop only starts one of them. For the sampling-offer to
   actually fire LiteLLM, the Ollama stack must be up too — there's
   no orchestrator that brings up both.

10. **Telegram surface is `apps/crabcc-hitl-agent`** (ollama-stack compose; Rust `crabcc-telegram` removed)
    and has its own Dockerfile / `crabcc-shared` network attachment.
    None of the audit above touches it — flagged for completeness
    because it's a fourth agent surface (Telegram bot → HITL agent →
    LiteLLM) that the user might assume is in scope.

## 9. Glossary

- **MCP** — Model Context Protocol, the JSON-RPC 2.0 wire format used
  by Claude Code and others to expose tools/resources/prompts to LLM
  agents. Stdio is canonical; HTTP / streamable-HTTP are also defined.
  See `crates/crabcc-mcp/src/lib.rs:1-10`.
- **AgentRuntime** — Rust trait at
  `crates/crabcc-cli/src/agent.rs:151`. Three impls today:
  `SubprocessRuntime` (host fork+exec), `SandboxRuntime` (microVM stub
  behind `agent-sandbox` feature), `BullmqRuntime` (Redis-queued Docker
  spawn behind `agents-bullmq` feature).
- **BullMQ job** — a JSON document `AgentJob` (`agent_bullmq.rs:46`)
  enqueued onto the `crabcc:agents` Redis list by the host CLI;
  consumed by the `crabcc-agents` worker; per-job stdout/stderr
  streamed back via `crabcc:agent:logs:<job_id>` Redis Stream.
- **Apple `container`** — Apple's first-party
  Virtualization.framework-backed OCI runtime introduced in 2025;
  drop-in for Docker on macOS arm64. Wired into
  `install/internal-agents/compose.yml` for the per-crate fan-out
  path. Not wired into `BullmqRuntime`.
- **LiteLLM** — `ghcr.io/berriai/litellm:main-stable`; an OpenAI-shaped
  proxy that fronts multiple model providers. In `install/ollama-stack`
  it sits at `:4000` and forwards Ollama-bound calls through Caddy.
- **Sampling-offer** — MCP `sampling/createMessage` flowing the
  *opposite* direction of normal: a connected MCP server asks its host
  to run an LLM completion. The desktop's `LiteLlmSamplingHandler`
  (`sampling.rs:466`) fulfils these via the local LiteLLM proxy so
  containerised agents and external MCP servers don't need their own
  API keys. See `MCP-SAMPLING-OFFER.md`.
- **Sandboxed Docker** — `BullmqRuntime`'s posture: distroless
  agent-runner image, `cap_drop=ALL`, `readonly_rootfs`, tmpfs-only
  writable paths, `network=none` by default,
  `security_opt=no-new-privileges`, `pids_limit=256`, 2 GiB memory
  cap, `--init` (tini PID1). Threat model in
  `apps/CONTAINER-POLICY.md` §"Trust boundary today" /
  `install/agent-runtime.md` §"v3.0".

---

## Appendix A — Workspace excluded crates

`Cargo.toml:13` lists two crates excluded from `cargo build --workspace`:

- `apps/crabcc-hitl-agent` — Python HITL service (Telegram + LiteLLM); built via
  `install/ollama-stack` compose, not `cargo build --workspace`.
- `crates/crabcc-desktop` — gpui-component pulls `tree-sitter = "0.25"`,
  conflicts with `crabcc-core` on 0.22. Standalone build only. **All
  desktop MCP work lives here**, so none of §4 row 6 / §7.2 is reachable
  via a default workspace `cargo` invocation.

## Appendix B — File:line index of the load-bearing claims

| Claim | File | Line |
|---|---|---|
| `AgentRuntime` trait | `crates/crabcc-cli/src/agent.rs` | 151 |
| `SubprocessRuntime::run` cwd to req.root | `crates/crabcc-cli/src/agent.rs` | 342 |
| `SubprocessRuntime` env passthrough including OLLAMA_* | `crates/crabcc-cli/src/agent.rs` | 360 |
| `agent::run` dispatches transports | `crates/crabcc-cli/src/agent.rs` | 783 |
| `ollama_stack::ensure_up` invocation | `crates/crabcc-cli/src/agent.rs` | 772 |
| `BullmqRuntime` impl | `crates/crabcc-cli/src/agent_bullmq.rs` | 81 |
| `AgentJob` shape (no `root` field) | `crates/crabcc-cli/src/agent_bullmq.rs` | 46 |
| `Runner::run` create_container | `apps/crabcc-agents/src/runner.rs` | 96 |
| `Runner::host_config` mounts | `apps/crabcc-agents/src/runner.rs` | 214 |
| `Runner::compose_env` | `apps/crabcc-agents/src/runner.rs` | 286 |
| Worker bind-mount of docker.sock | `apps/crabcc-agents/docker-compose.yml` | 52 |
| Agent-runner Dockerfile claude install | `apps/crabcc-agents/agent-runner/Dockerfile` | 57 |
| Agent-runner entrypoint MCP wiring | `apps/crabcc-agents/agent-runner/entrypoint.sh` | 81–96 |
| Agent-runner mcp.json (axint) | `apps/crabcc-agents/agent-runner/mcp.json` | 3 |
| Ollama stack compose | `install/ollama-stack/docker-compose.yml` | 39–200 |
| Ollama stack mounts host `.crabcc:ro` | `install/ollama-stack/docker-compose.yml` | 61 |
| LiteLLM compose service | `install/ollama-stack/docker-compose.yml` | 149 |
| Caddy compose service | `install/ollama-stack/docker-compose.yml` | 108 |
| `crabcc-mcp` stdio entry | `crates/crabcc-mcp/src/lib.rs` | 42 |
| `crabcc-mcp` HTTP entry | `crates/crabcc-mcp/src/lib.rs` | 150 |
| HITL `chat` MCP tool | `apps/crabcc-hitl-agent/src/crabcc_hitl/mcp_server.py` | 36 |
| HITL streamable-HTTP path | `apps/crabcc-hitl-agent/src/crabcc_hitl/mcp_server.py` | 51 |
| `LiteLlmSamplingHandler` | `crates/crabcc-desktop/src/sampling.rs` | 326 |
| `LiteLlmSamplingHandler::handle` | `crates/crabcc-desktop/src/sampling.rs` | 467 |
| Default LiteLLM endpoint | `crates/crabcc-desktop/src/sampling.rs` | 31 |
| `run_test_sampling` smoke | `crates/crabcc-desktop/src/state.rs` | 1261 |
| Desktop `services::ensure_stack_started` | `crates/crabcc-desktop/src/services.rs` | 105 |
| Desktop compose-up command | `crates/crabcc-desktop/src/services.rs` | 133 |
| Internal-agents compose mount | `install/internal-agents/compose.yml` | 22–30 |
| Internal-agents Containerfile entry | `install/internal-agents/Containerfile` | 59 |
| `crabcc-shared` network bootstrap | `install/init-shared-network.sh` | 56 |
| HITL compose attaches to crabcc-shared | `apps/crabcc-hitl-agent/docker-compose.yml` | 51 |
| MCP-NATIVE planned tool table | `crates/crabcc-desktop/docs/MCP-NATIVE.md` | 79 |
| MCP-NATIVE sampling section | `crates/crabcc-desktop/docs/MCP-NATIVE.md` | 108 |
| Sampling-offer trust boundary | `crates/crabcc-desktop/docs/MCP-SAMPLING-OFFER.md` | 18 |
| Consent identity table | `crates/crabcc-desktop/docs/MCP-CONSENT.md` | 47 |
| Desktop workspace-exclusion note | `Cargo.toml` | 18 |
