# Runner cache-proxy (generic LRU+TTL)

A tiny stdlib-only caching reverse-proxy for the self-hosted runner layer. It
sits on `127.0.0.1` in front of **one** upstream and caches responses so
repeated agent API calls within a short window are served from memory instead
of re-hitting (and re-paying for) the upstream.

**DEFAULT-OFF.** Nothing here is wired into `install.sh`. It runs only if you
install the systemd unit below. No production behaviour changes until you opt in.

## What it does

- **LRU + TTL** in-memory cache (`OrderedDict`, capacity-bounded).
- **Dynamic TTL = `avg_job_runtime × 3 + 60s`**, recomputed from a rolling
  window of job durations reported via the control endpoints. Falls back to
  `CACHE_PROXY_TTL_FALLBACK` (default 300s) until the first job finishes.
- **HIT/MISS per request** → stdout (journald / runner log), with an
  `X-Cache: HIT|MISS` response header.
- **HIT/MISS per job** → printed on job end *and* returned in the
  `/__cache/job/end` JSON so the job step can echo it into its own job log.
- **Key = method + path + sha256(body) + sha256(Authorization)** — different
  API keys never share a bucket; auth is hashed, never logged.
- **Fail-open:** if the upstream errors, it returns `502` and never serves a
  stale entry in its place.

## Config (env)

| Var | Default | Meaning |
|---|---|---|
| `CACHE_PROXY_UPSTREAM` | *(required)* | e.g. `https://api.anthropic.com` |
| `CACHE_PROXY_LISTEN` | `127.0.0.1:8899` | bind address |
| `CACHE_PROXY_CAPACITY` | `256` | max LRU entries |
| `CACHE_PROXY_TTL_FALLBACK` | `300` | seconds, before job stats exist |
| `CACHE_PROXY_METHODS` | `GET,POST` | methods eligible for caching |

## Enable on a runner (opt-in)

```ini
# /etc/systemd/system/cache-proxy.service  (see cache-proxy.service.example)
[Service]
Environment=CACHE_PROXY_UPSTREAM=https://api.anthropic.com
ExecStart=/usr/bin/python3 /opt/actions-runner/cache-proxy/cache_proxy.py
Restart=always
```
```bash
sudo systemctl enable --now cache-proxy
journalctl -u cache-proxy -f      # runner log: per-request HIT/MISS
```

## Route a job through it

Point the agent at the proxy and bracket the work with start/end pings so the
per-job tally and the TTL stats are recorded. Example for an Anthropic-backed
step (`job-wrapper.sh.example`):

```bash
JOB="${GITHUB_RUN_ID}-${GITHUB_JOB}"
curl -s -XPOST 127.0.0.1:8899/__cache/job/start -d "{\"job_id\":\"$JOB\"}"

# Make the agent's SDK hit the proxy instead of the API directly:
export ANTHROPIC_BASE_URL=http://127.0.0.1:8899
# (the SDK must also send `X-Cache-Job: $JOB`; if it can't set custom headers,
#  the cache still works — only the per-job tally is attributed to "(none)".)

# ... run pr-agent / claude-code-action ...

# Echo the tally into THIS job's log:
curl -s -XPOST 127.0.0.1:8899/__cache/job/end -d "{\"job_id\":\"$JOB\"}" \
  | tee /dev/stderr
```

## Caveats

- Caching **POST/LLM** responses collapses an identical prompt to one answer
  for the TTL window. That's the point for re-runs, but don't enable it where
  you *want* fresh sampling each call.
- Only `2xx` responses are cached.
- In-memory only — a proxy restart is a cold cache (fine; it just re-warms).

## Local smoke test

```bash
CACHE_PROXY_UPSTREAM=https://example.com python3 cache_proxy.py &
curl -s -XPOST 127.0.0.1:8899/__cache/job/start -d '{"job_id":"t"}'
curl -s -D- 127.0.0.1:8899/  | grep -i x-cache    # MISS
curl -s -D- 127.0.0.1:8899/  | grep -i x-cache    # HIT
curl -s -XPOST 127.0.0.1:8899/__cache/job/end -d '{"job_id":"t"}'
```
