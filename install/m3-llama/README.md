# m3 llama pool (load balancer + admission control)

On a 36 GB M3 Max, **do not** run multiple copies of `qwen-coder-32b` — that OOMs.
Use **one** `llama-server` with `--parallel N` (shared weights, N KV slots) and an
HTTP proxy on `:8080` that caps inflight + queued requests.

```
  opencode / orchestrator / SSH forward
              │
              ▼
        lb-proxy.py :8080     ← LLAMA_MAX_INFLIGHT=2, LLAMA_MAX_QUEUE=6
              │
              ▼
   llama-server :18080       ← -np 2, -c 8192, 127.0.0.1 only
```

## Deploy to m3

From this repo:

```bash
./scripts/deploy-m3-llama.sh
ssh m3-task 'cd /opt/plodri/llama-server && ./lb/pool.sh restart'
```

## Ops on m3

```bash
cd /opt/plodri/llama-server
./lb/pool.sh status
./lb/pool.sh logs
./lb/pool.sh stop
./lb/pool.sh start    # LLAMA_MODEL=qwen-coder-14b ./lb/pool.sh start
```

**Note:** Tailscale often binds the tailnet IP on `:8080`. The LB listens on
`127.0.0.1:8080` only; use SSH `LocalForward` or local opencode on m3.

Tune in `lb/env.local` (not committed):

```bash
export LLAMA_PARALLEL=2
export LLAMA_MAX_INFLIGHT=2
export LLAMA_CTX=8192
```

## Client (laptop)

Unchanged: `Host m3` → `LocalForward 8080 127.0.0.1:8080`, health on
`http://127.0.0.1:8080/health`, dispatch via `./scripts/dispatch-m3.sh`.

Queue visibility: `curl http://127.0.0.1:8080/lb/status` (via forward), or
`./scripts/m3-pool-status.sh` from this repo.

### Orchestrator / fan-out

The pool allows **2 inflight** + **6 queued** LLM requests. Keep concurrent
`opencode run -m local-llama/...` jobs at **≤2** on m3 or work piles up behind
the LB while clients look hung. Example:

```bash
export M3_LLAMA_MAX_PARALLEL=2   # wire into your orchestrator dispatch
```
