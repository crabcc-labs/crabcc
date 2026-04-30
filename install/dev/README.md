# crabcc dev Compose stack

Local-dev convenience stack — multi-stage build of `crabcc` itself + a
fast-reload bun container running `esbuild --watch` against the
`crabcc-viz/web/` React frontend. Issues #105 / #107.

## Layout

| service | image / source | purpose |
|---|---|---|
| `crabcc` | `install/Dockerfile.crabcc` (multi-stage) | runs `crabcc serve` on `:8090` against the bind-mounted repo |
| `viz-web` | `oven/bun:1-alpine` | runs `bun run dev` (esbuild watch) on `crates/crabcc-viz/web/` |

The repo root is bind-mounted into `crabcc` at `/workspace` (read-write so
`crabcc index` writes `.crabcc/` back to the host). The viz-web subtree is
bind-mounted standalone so the watch surface stays tight.

## Quick start

```bash
# from repo root
docker compose -f install/dev/docker-compose.yml up --build

# in a second terminal — verify
curl -s http://localhost:8090/healthz
```

Edit `crates/crabcc-viz/web/src/*.tsx` on the host. esbuild --watch rebuilds
`dist/live.html` in the bind volume; until issue #107 lands the disk-read
dev mode, trigger a `task build-fast` on the host to refresh `crabcc serve`'s
embedded HTML.

## Combining with the Ollama auth stack

Both compose files are independent — pass two `-f` flags to bring them up
together:

```bash
docker compose \
  -f install/ollama-stack/docker-compose.yml \
  -f install/dev/docker-compose.yml \
  up --build
```

`crabcc` reaches LiteLLM at `http://litellm:4000` (cross-stack DNS resolution
via the `stack` and `dev` Compose networks — Docker links them automatically
when both files are loaded).

## Image cache mounts

`Dockerfile.crabcc` uses BuildKit cache mounts for the cargo registry, git
deps, and the target tree. Iterative rebuilds (changing one file in
`crates/crabcc-cli/src/main.rs`) take ~10–20 s instead of ~2 min cold.

Enable BuildKit if not on by default:

```bash
export DOCKER_BUILDKIT=1
```

## Resource caps

Each service has a `deploy.resources.limits.memory` cap so a single
out-of-control inference run / runaway esbuild process won't OOM the host.

| service | memory cap |
|---|---|
| `crabcc`  | 2 GiB |
| `viz-web` | 1 GiB |

Override per-shell:

```bash
COMPOSE_PROFILES=heavy docker compose -f install/dev/docker-compose.yml up
```

(Profile gating for heavier workloads lands in a follow-up.)

## Out of scope (delegated)

- Browser-side livereload (Server-Sent Events from esbuild → browser) → issue #107.
- Multi-arch buildx CI workflow → bundled with the menubar app's release pipeline (issue #107 Part A).
- Production Compose stack with TLS / external DB / horizontal scaling — this stack is **dev only**.
