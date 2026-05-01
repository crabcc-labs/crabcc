# crabcc-agents

BullMQ-driven Claude Code agent runner. Pulls jobs from a Redis-backed
queue (BullMQ), spawns one sandboxed Docker container per job, and
streams the agent's stdout/stderr back through Redis Streams for
real-time consumption by the dashboard.

## Architecture

```
producers           ┌─────────────┐         ┌─────────────────────────┐
(telegram bot,  →   │  BullMQ     │   →     │  crabcc-agents (Rust)   │
 dashboard,         │  queue      │         │  • bullmq-rs Worker     │
 MCP, …)            │  (Redis)    │         │  • bollard Docker SDK   │
                    └─────────────┘         │  • Redis Streams XADD   │
                                            └────────────┬────────────┘
                                                         │ docker run
                                                         ▼
                                            ┌─────────────────────────┐
                                            │  crabcc-agent-runner    │
                                            │  (alpine + tini)        │
                                            │  • claude code -p ...   │
                                            │    --sandbox            │
                                            │  • rtk (token killer)   │
                                            │  • context-mode plugin  │
                                            └────────────┬────────────┘
                                                         │ stdout/stderr
                                                         ▼
                                            ┌─────────────────────────┐
                                            │  Redis Streams          │
                                            │  crabcc:agent:logs:{id} │
                                            └────────────┬────────────┘
                                                         │ XREAD BLOCK
                                                         ▼
                                                  dashboard / web UI
```

External primitives:

* **BullMQ-rs** — https://github.com/bogardt/bullmq-rs (Rust port of BullMQ).
* **Claude Code sandboxing** — https://code.claude.com/docs/en/sandboxing.
* **context-mode** — https://github.com/mksglu/context-mode (keeps tool
  output out of the model's context window).
* **RTK** — https://github.com/rtk-ai/rtk (transparent CLI proxy that
  rewrites token-heavy invocations).
* **axint** — https://github.com/agenticempire/axint (TS/Python → Swift
  compiler with a Fix-Packet repair loop and 33 MCP tools; baked into
  the runner image and registered via `/etc/crabcc-agent/mcp.json` so
  every agent can compile, validate, and repair Apple-native code).
* **Redis Streams** —
  https://redis.io/docs/latest/develop/data-types/streams/ /
  https://redis.io/tutorials/howtos/solutions/streams/streaming-llm-output/.

## Job payload (`AgentJob`)

```json
{
  "prompt": "audit src/api for SSRF",
  "kind": "claude-code",
  "model": "claude-sonnet-4-6",
  "effort": "high",
  "sandbox": { "network": false, "writeable_root": false, "bash": true },
  "env": {},
  "timeout_secs": 600
}
```

  - `kind` selects the agent CLI inside the container. Two values:
    - `claude-code` (default) — full Claude Code surface, MCP tools,
      sandbox flag, SSO auth pass-through.
    - `mini-swe` — [mini-swe-agent](https://mini-swe-agent.com), the
      ~100 LoC Python SWE agent. Useful for narrow "fix this failing
      test" jobs. Reads `/etc/crabcc-agent/mini.yaml` for defaults
      (step limit 50, cost limit $5, model `claude-sonnet-4-6`,
      cwd `/workspace`), all CLI-overridable per job.
  - `model` / `effort` are optional. They fall back to
    `AGENT_DEFAULT_MODEL` (Sonnet 4.6) and `AGENT_DEFAULT_EFFORT`
    (`high`) on the worker.

Producers push these as the `data` of a BullMQ job on queue
`crabcc:agents` (configurable via `AGENTS_QUEUE`).

## Auth

Auth carries through from the **host's Claude Code SSO session** rather
than per-job API keys. The worker bind-mounts the host's
`~/.claude/.credentials.json` read-only into every agent container at
`/home/nonroot/.claude/.credentials.json`, so the in-container `claude`
CLI sees the same logged-in user (and the same OAuth refresh path)
without any token ever touching env vars or job payloads.

  - Configure the host path via `HOST_CLAUDE_CREDENTIALS` (defaults to
    `$HOME/.claude/.credentials.json`). Set to `-` or `none` to disable.
  - On hosts where credentials live only in Keychain (macOS), set
    `CLAUDE_CODE_OAUTH_TOKEN` instead — extracted with `claude setup-token`.
    The token is only injected when no credentials file is mounted, so a
    stale token can never shadow a fresh file.

## MCP servers in the runner

The image ships with a static `--mcp-config` at
`/etc/crabcc-agent/mcp.json` so MCP wiring is reproducible per image
tag and never depends on first-run approval flows that would clash
with the read-only rootfs. The default config registers:

  - **axint** (`axint mcp`) — Apple-native compile / validate / fix /
    schema / templates / fix-packet tools.

Add more by editing `agent-runner/mcp.json` and rebuilding the image.

### mini-swe-agent

Pinned to a specific release via `MINI_SWE_VERSION` arg in
`agent-runner/Dockerfile` (default `1.7.0`). Image pre-creates a
read-only config at `/etc/crabcc-agent/mini.yaml`; per-job
overrides via env-driven CLI flags (entrypoint composes them):

| Env                  | Maps to            | Notes |
|----------------------|--------------------|-------|
| `CLAUDE_MODEL`       | `--model`          | Defaults to `claude-sonnet-4-6` |
| `MINI_STEP_LIMIT`    | `--step-limit`     | Default 50 from yaml |
| `MINI_COST_LIMIT`    | `--cost-limit`     | Default $5 from yaml; mini aborts on overrun |
| `ANTHROPIC_API_KEY`  | (consumed by litellm) | Required for Anthropic models — pass via job's `env` |

The entrypoint always passes `--yolo` (mini-swe-agent never prompts
in container — stdin is closed, tty is off). All actual sandboxing is
enforced at the Docker layer (`cap-drop=ALL`, read-only rootfs, etc.)
— mini's local environment is the agent container itself.

### axint via host (HTTP) instead of in-container (stdio)

Set `HOST_AXINT_MCP_URL` on the worker to delegate axint MCP calls to
an `axint-mcp-http` running on the host. Agents share the host's warm
caches (project memory pack, registry, fix-packet history) and don't
pay a cold-start per job.

```bash
# on the host
axint-mcp-http --port 7785

# worker env
HOST_AXINT_MCP_URL=http://host.docker.internal:7785/mcp
AGENT_NETWORK=bridge   # default; the worker auto-injects this when
                       # HOST_AXINT_MCP_URL is set.
```

When `HOST_AXINT_MCP_URL` is set:

  - The worker switches the agent's network from `none` to the
    configured `AGENT_NETWORK` (default `bridge`). To narrow the
    egress blast radius, point this at a dedicated docker network
    that only resolves the host axint port.
  - The worker adds `--add-host host.docker.internal:host-gateway`
    so the in-container URL resolves on Linux ≥ 20.10 and on Docker
    Desktop without further wiring.
  - The entrypoint generates a fresh `/tmp/mcp.json` with `type:
    "http"` instead of using the baked stdio config; the in-container
    `axint` bin stays installed but unused.

## What's NOT in the runner image

To keep cold-start fast and the attack surface small, the agent-runner
image deliberately omits:

  - Chrome / Chromium / Playwright / Puppeteer / any browser binary.
  - System compilers (gcc/clang).
  - Any GUI toolchain.

A separate image variant can ship a browser layer if a future job class
needs it; the default agents stay headless.

## Container hardening

The worker spawns each agent container with:

| Knob                | Value (default)            | Why |
|---------------------|----------------------------|---|
| `--init`            | on                         | Docker tini reaps zombies. |
| `--read-only`       | on (unless overridden)     | Block writes outside tmpfs. |
| `tmpfs /workspace`  | 1 GiB                      | Fast scratch, no disk I/O. |
| `tmpfs /tmp`        | 512 MiB                    | No leakage to host disk. |
| `--shm-size`        | 256 MiB                    | Bigger than 64 MiB default — Claude Code IPC. |
| `--memory`          | 2 GiB                      | Hard cap; OOM kills the agent, not the host. |
| `--cpus`            | 2.0 (`quota/period`)       | Predictable throughput. |
| `--pids-limit`      | 256                        | Fork-bomb guard. |
| `--cap-drop=ALL`    | on                         | No CAP_NET_ADMIN, etc. |
| `--security-opt no-new-privileges` | on              | No setuid escalation. |
| `--ipc=private`     | on                         | Faster /dev/shm than namespaced sharing. |
| `--network=none`    | on (unless `sandbox.network=true`) | No exfil path. |

A second layer of zombie-reaping lives inside the image (`tini` as
`ENTRYPOINT`), in case `--init` is omitted by an out-of-band caller.

## Healthcheck

Service exposes `GET /healthz` on `:9090` (configurable). Returns
`200 OK` only when the Docker daemon and Redis are both reachable.

## Log streaming

For each job, stdout+stderr is `XADD`'d into
`crabcc:agent:logs:{jobId}` with fields `s` (source: stdout/stderr/event),
`t` (RFC3339 timestamp), `m` (message). The stream is approximately
trimmed to `STREAM_MAXLEN` entries (default 10 000). On container exit
the worker writes a sentinel `s=event m=__eof__ exit=<code>` so blocking
consumers can stop cleanly.

Reference pattern:
https://redis.io/tutorials/howtos/solutions/streams/streaming-llm-output/

## Quick start (host-key propagation)

```bash
./apps/crabcc-agents/scripts/start.sh
```

Resolves `ANTHROPIC_API_KEY` from (in order) the env, your host's
`~/.claude/.credentials.json`, then macOS Keychain (Claude Code
stores the OAuth payload there on darwin), exports it, and runs
`task up` — which kicks `litellm:check-build-run` first to ensure
the proxy is alive, then brings the agents stack up.

`scripts/extract-anthropic-key.sh` is the standalone extractor — it
prints the bare key to stdout and a redacted (`N chars`) summary to
stderr, never logs the value. Reuse it in your own scripts.

## Trackability headers

Producers can attach key/value headers that propagate end-to-end:

```bash
CRABCC_HEADER_X_SOURCE=live-web \
CRABCC_HEADER_X_REQUEST_ID=req-abc-123 \
  cargo run --bin seed -- "audit src/api"
```

Where they go:

| Stage                 | Form                                                |
|-----------------------|-----------------------------------------------------|
| BullMQ job payload    | `headers: {"x-request-id":"..."}` field            |
| Redis Stream (entry 2)| `s=event m=headers <json>` right after `container started` |
| Agent container env   | `CRABCC_HEADER_X_REQUEST_ID=...` (upper-snake-cased) |
| LiteLLM forward       | When agents pass them as HTTP request headers, the proxy's `forward_client_headers_to_llm_api: true` ships them to Anthropic — closing the loop on log correlation |

Stream consumers can detect headers without parsing every line: the
**second** entry of every stream is either `headers <json>` (when
provided) or it's missing (skipped silently for empty maps). The
JSON parses without further unwrapping — `m.split(' ', 2)[1]`.

Convention: lower-case keys with `-`, HTTP-header style. Common ones:
`x-source` (`cli`/`telegram`/`live-web`/`pr-bot`), `x-request-id`,
`x-trace-id`, `x-job-run-id`. The `crabcc agent run` BullMQ runtime
(behind `agents-bullmq`) auto-fills `x-source=cli` and
`x-job-run-id=<run.id>`.

**Don't put PII here** — headers ride through Redis logs, container
env, and upstream LLM logs. Use opaque correlation ids instead of
usernames / emails / session ids.

## Service discovery

URLs (Redis, LiteLLM) go through the same resolver pattern as
`crabcc_core::service_discovery`. Resolution order, highest wins:

1. The service's explicit env var (`REDIS_URL`, `LITELLM_BASE_URL`).
2. `CRABCC_COMPOSE=1` → compose-network host (`redis`, `litellm`) +
   default port. The repo's docker-compose stack sets this on the
   worker, so the same binary runs unchanged inside or outside compose.
3. Localhost + default port.

The worker logs the resolution at boot:

```
crabcc-agents: discovery — redis@redis://127.0.0.1:6379 (local-default), \
litellm@http://127.0.0.1:4000 (local-default)
```

`source` shows which rule won (`env` / `compose-default` / `local-default`),
so a misconfigured deploy is one log line away from being obvious.

The local resolver lives in `src/discovery.rs` — kept as a small
replica rather than a path-dep on crabcc-core, since this crate is
standalone for build-isolation reasons. The canonical contract is
[`crates/crabcc-core/src/service_discovery.rs`](../../crates/crabcc-core/src/service_discovery.rs);
bumps there should mirror here.

## LiteLLM preflight + check-build-run

The worker preflights `LITELLM_BASE_URL` at boot — fast TCP connect
with a 1.5 s timeout. Outcomes:

| Mode                      | Reachable | Unreachable |
|---------------------------|-----------|-------------|
| `LITELLM_REQUIRED` unset  | log info  | log warn, continue |
| `LITELLM_REQUIRED=1`      | log info  | hard-fail boot |

Bringing the LiteLLM stack up is owned by the existing
`install/ollama-stack/` (issue #105) — we deliberately don't duplicate
the container spec. The per-crate Taskfile wraps it for a single
idempotent command:

```bash
task -d apps/crabcc-agents litellm:check-build-run
```

That target probes first, exits early when LiteLLM is already
serving, otherwise runs `bash install/ollama-stack/start.sh` (which
brings up ollama + caddy + litellm with `--wait` health gating) and
re-checks. Companion targets:

  - `litellm:check` — probe only (non-zero exit when down)
  - `litellm:build-run` — force the bring-up
  - `litellm:down` — tear the stack down
  - `litellm:logs` — tail the litellm container

## Redis tuning

Redis is tuned for two patterns: BullMQ's long-blocking `BLPOP` calls
on the queue, and frequent small `XADD`s (one per agent stdout line)
into MAXLEN-trimmed streams. The full config is in
[`redis/redis.conf`](./redis/redis.conf); the load-bearing knobs:

| Knob                      | Value         | Why |
|---------------------------|---------------|---|
| `maxmemory-policy`        | `noeviction`  | Eviction would silently drop queued jobs. Stream growth is bounded by `MAXLEN ~` in the worker; nothing else can grow without bound. |
| `appendonly` + `everysec` | yes / 1s      | ≤1 s job loss across restarts. RDB snapshots disabled (`save ""`) — no fork stalls. |
| `lazyfree-*`              | yes           | Async free for big stream nodes / dropped queues. Keeps the event loop unblocked. |
| `timeout`                 | 0             | BullMQ workers hold BLPOP forever; non-zero would kill them. |
| `tcp-keepalive`           | 30 s          | Detect dead workers fast so their reservations don't park jobs. |
| `io-threads` / `do-reads` | 4 / yes       | Halve command latency under contention. Tune to host cores. |
| `stream-node-max-entries` | 256           | Smaller stream nodes → faster MAXLEN trim cycles. |
| `hz` / `dynamic-hz`       | 100 / yes     | Tighter expiration + dead-client cleanup. |

Run with the tuned config:

```bash
docker run -d --name crabcc-redis --restart=unless-stopped --init \
    -p 127.0.0.1:6379:6379 \
    -v "$PWD/apps/crabcc-agents/redis/redis.conf:/usr/local/etc/redis/redis.conf:ro" \
    -v crabcc-redis-data:/data \
    --sysctl net.core.somaxconn=4096 \
    --ulimit nofile=65536:65536 \
    redis:7-alpine \
    redis-server /usr/local/etc/redis/redis.conf
```

Or the whole stack via `docker-compose.yml` in this directory:

```bash
docker compose -f apps/crabcc-agents/docker-compose.yml up -d
```

## Cold start

Cold start = wall-clock latency from a job hitting Redis to the first
agent stdout line being readable on the stream. Measured with the
smoke-mode pipeline (alpine + shell echo, no Anthropic call):

| Config                       | Cold start (p50) |
|------------------------------|------------------|
| `AGENTS_POLL_MS=50` (default)| ~225 ms          |
| `AGENTS_POLL_MS=10`          | ~210 ms          |

The remaining floor (~200 ms) is dominated by `docker create` +
`docker start` daemon round-trips and is hard to reduce without a
warm-container pool (intentionally out of scope — the per-job
isolation guarantees are easier to reason about with one container
per job).

The tuning knobs that exist:

  - `AGENTS_POLL_MS` — bullmq-rs polls with `sleep(poll_interval)`
    between `ZPOPMAX` checks. Lower = faster pickup, more idle CPU.
    Default 50 ms.
  - `AGENTS_PREWARM=1` — at boot, inspect the agent image and pull
    only if missing. Eliminates the first-job pull stall and surfaces
    a typo'd `AGENT_IMAGE` immediately. On by default.
  - `AGENTS_TOKIO_THREADS` — tokio worker thread cap. Default
    `min(host_cores, 4)`. Worker is IO-bound; more threads ≠ more
    throughput.
  - `MALLOC_CONF` (Linux container) — `narenas:1,tcache:true` for
    smaller arena footprint and faster small-alloc paths;
    `dirty_decay_ms:5000,muzzy_decay_ms:5000` to return memory faster.
    Set as `ENV` in the worker Dockerfile.

## Build performance

The worker's build stage uses the [wild](https://github.com/wild-linker/wild)
linker on Linux targets. It's pinned via `WILD_VERSION` in the
Dockerfile and installed in a dedicated cached layer; the cost is paid
once per wild bump, not per code change. Linker config lives in
`.cargo/config.toml` and is scoped to Linux targets only — macOS dev
builds keep using Apple's ld.

## Build & run

```bash
# build the worker image
docker buildx build -f apps/crabcc-agents/Dockerfile \
    -t ghcr.io/peterlodri-sec/crabcc-agents:dev --load apps/crabcc-agents

# build the runner image (the thing the worker spawns)
docker buildx build -f apps/crabcc-agents/agent-runner/Dockerfile \
    -t ghcr.io/peterlodri-sec/crabcc-agent-runner:dev --load \
    apps/crabcc-agents/agent-runner

# run worker (needs Docker socket)
docker run -d --name crabcc-agents --restart=unless-stopped --init \
    -v /var/run/docker.sock:/var/run/docker.sock \
    --env-file apps/crabcc-agents/.env \
    -p 9090:9090 \
    ghcr.io/peterlodri-sec/crabcc-agents:dev
```

## Status

Phase 0 scaffold. The frontend wiring (live log pane, job lifecycle in
ActivityPanel) is tracked in a separate issue — see PR description.
