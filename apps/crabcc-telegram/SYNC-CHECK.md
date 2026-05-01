# SYNC-CHECK: telegram bot ↔ host wiring

Last run: 2026-05-01 by `scripts/telegram-up.sh` end-to-end test.

## Env-var source-of-truth

| Var | Owner | Notes |
|---|---|---|
| `TELEGRAM_BOT_TOKEN` | `apps/crabcc-telegram/.env` | passed at run time via `--env-file`; never baked into image |
| `CRABCC_PUBLIC_URL` | `scripts/telegram-up.sh` (`-e` override) | sourced from cloudflared at bring-up; the `.env` may carry a stale value but the `-e` flag wins because Docker exposes the *last* matching env entry to the process |
| `CRABCC_SERVE_URL` | `scripts/telegram-up.sh` (`-e` override) | hardcoded to `http://host.docker.internal:7878` so the bridge-network container can reach the host's `crabcc serve` |
| `OWNER_TELEGRAM_USER_ID` | **compile-time const** in `src/main.rs:62` (`5_875_395_828`) | env *cannot* widen this. `ALLOWED_TELEGRAM_IDS` was removed (footgun: empty == open). Doc strings in `main.rs:11–13` correctly call this out; root `Taskfile.yml:1551` still references the dead var — minor doc drift, not a security issue. |

## Observed drift

1. **Port mismatch (fixed in script)**. `crabcc serve` defaults to **7878** (`crates/crabcc-cli/src/main.rs:416`), but `scripts/cloudflared-tunnel.sh:27`, `Taskfile.yml:356`, the bot's `Config::from_env` default (`src/main.rs:84`), and several comments still hardcode **8090**. `telegram-up.sh` overrides via `PORT=$LOCAL_SERVE_PORT` (default 7878) and `CRABCC_SERVE_URL=http://host.docker.internal:7878`. Long-term: bump the bot's compile-time default to 7878 in a follow-up PR.
2. **Stale `.env` URL**. The committed-environment `.env` had a `CRABCC_PUBLIC_URL=…seekers-allowing-…` from a previous tunnel session. `telegram-up.sh` ignores it cleanly (overrides via `-e`); the stale line is harmless but worth pruning.
3. **Subprocess CLI inside distroless image**. `src/main.rs` shells out to `crabcc agent`, `crabcc agent-ls`, `crabcc memory search`, `crabcc index`, `crabcc doctor`. The runtime image is `gcr.io/distroless/cc-debian12:nonroot` — **no `crabcc` binary present**, so every CLI-shelling command (`/agent`, `/status`, `/doctor`, `/search`, `/kill`, `/index`) returns a "spawn crabcc: No such file or directory" error to the user. Only `/dashboard` (Mini App + `/api/agents` HTTP probe) works in the container today. The Dockerfile comment at line 22 already flags this: *"other bot commands that shell out to `crabcc` CLI need a future MCP-over-network refactor (tracked)."*

## MCP vs HTTP path

The bot currently uses **HTTP only** against `CRABCC_SERVE_URL` (`/api/agents`, `/api/bootstrap`, `/live` for Mini App). It does **not** touch the service-discovery layer (`crates/crabcc-core/src/service_discovery.rs`) at all — confirmed by `grep service_discovery apps/crabcc-telegram/src/main.rs` returning zero hits. The `services.json` sidecar already advertises a `crabcc-mcp` entry on `127.0.0.1:8091` (added in #205 / #206), but the bot does not consume it.

The endpoints the bot calls (`/api/agents`, `/api/bootstrap`, `/live`) all exist in `crates/crabcc-viz/src/lib.rs` (lines 162, 443, 451, 427) — no missing-endpoint drift. Verified end-to-end: `curl https://<tunnel>/api/bootstrap` returned a 200 with the live `crabcc serve` snapshot.

## Recommended next steps

- Migrate the bot's CLI-shelling commands to MCP-over-HTTP (port 8091 via `host.docker.internal:8091`); restores `/agent`, `/status`, `/doctor` etc. inside the distroless container.
- Bump the compile-time default `CRABCC_SERVE_URL` from `:8090` → `:7878` to match the CLI default.
- Drop the dead `ALLOWED_TELEGRAM_IDS` reference from root `Taskfile.yml:1551`.
- Consider auto-rewriting `CRABCC_PUBLIC_URL` in `.env` from `telegram-up.sh` to keep `task -d apps/crabcc-telegram run` (foreground, no override) consistent with the container path.
