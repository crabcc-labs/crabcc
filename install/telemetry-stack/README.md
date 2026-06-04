# crabcc telemetry stack (hardened, public)

A **separate, public-facing** observability backend for crabcc: OpenObserve
(single Rust binary — OTLP-native, built-in UI + SQL, Parquet storage) behind a
**Cloudflare tunnel** so the host opens **zero inbound ports**. Runs on its own
instance, isolated from [`install/ollama-stack`](../ollama-stack) so a
compromise here can't reach the inference stack.

```
dev mac / agent containers / CI                 ──OTLP/HTTP (gzip + Bearer)──┐
  crabcc built --features telemetry                                          │
  OTEL_EXPORTER_OTLP_ENDPOINT=https://telemetry.example.com/api/default      │
  OTEL_EXPORTER_OTLP_HEADERS=Authorization=Basic <b64(user:pass)>            ▼
                                                          cloudflared (outbound only)
                                                                  │
                                                                  ▼
                                                          openobserve:5080
                                                          (UI + SQL + Parquet)
```

## Deploy

1. **Provision a dedicated small instance** (Hetzner CX22 is plenty — OpenObserve
   idles under ~100 MB). Nothing else runs on it.
2. `cp .env.example .env` and fill in `ZO_ROOT_USER_PASSWORD` (long + unique) and
   `CLOUDFLARE_TUNNEL_TOKEN`.
3. In **Cloudflare Zero Trust → Networks → Tunnels**, create a named tunnel,
   copy its token into `.env`, and add a **public hostname** (e.g.
   `telemetry.example.com`) routing to `http://openobserve:5080`.
4. `docker compose up -d`
5. Confirm: open `https://telemetry.example.com` → OpenObserve login. There is
   **no** open port on the box (`ss -tlnp` shows nothing public) — only the
   tunnel's outbound connection.

## Point crabcc clients at it

crabcc must be built with the telemetry feature (the shipped build is —
`task build` / `task install` enable `telemetry`). Then per client:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="https://telemetry.example.com/api/default"
# Basic auth = base64("<user>:<pass>"); prefer a dedicated ingest user (below).
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic $(printf '%s' 'ingest@example.com:PASS' | base64)"
```

crabcc's exporter posts gzip'd OTLP/JSON spans to `${ENDPOINT}/v1/traces` with
those headers (see `crates/crabcc-cli/src/telemetry.rs`). Without
`OTEL_EXPORTER_OTLP_HEADERS` the request is unauthenticated and OpenObserve will
reject it — which is the point.

## Hardening checklist

- [ ] **Tunnel-only.** No `ports:` in the compose; the host firewall denies all
      inbound. The tunnel is the *only* path in.
- [ ] **Auth required for ingest.** Clients must send the `Authorization` header
      (the exporter now supports `OTEL_EXPORTER_OTLP_HEADERS`); anonymous OTLP is
      refused.
- [ ] **Dedicated ingest identity.** Create a non-root OpenObserve user scoped to
      ingest and use *its* credentials in `OTEL_EXPORTER_OTLP_HEADERS`, so rotating
      a leaked client token never touches the admin login.
- [ ] **Strong, managed root password.** Not in git; rotate on exposure.
- [ ] **Add Cloudflare Access** in front of the UI hostname (SSO / email OTP) so
      the dashboard isn't world-reachable even over the tunnel — ingest can stay
      on a separate token-authenticated hostname/path.
- [ ] **Isolated instance.** Not colocated with ollama-stack/litellm/agents.
- [ ] **Bounded retention** via `ZO_RETENTION_DAYS` (default 30) so a public
      ingest endpoint can't fill the disk indefinitely.
- [ ] **Watch ingest volume** — consider Cloudflare rate-limiting rules on the
      ingest hostname to blunt abuse.

> Note: exact OpenObserve env/paths (`/api/<org>`, ingest tokens) can shift
> between versions — verify against the image you pin. This stack is the
> declarative starting point; you own the running instance.
