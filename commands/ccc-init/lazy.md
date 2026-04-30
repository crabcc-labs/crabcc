---
description: Lazy full-bootstrap — index + graph + memory + aliases + tools + live view + ollama walkthrough + upgrade check + marker file. Drop this in a fresh repo and walk away.
---

# `/ccc-init:lazy` — full crabcc + ccc bootstrap

You are bootstrapping a complete crabcc + ccc environment for the user's
**current working directory**. Execute end-to-end without confirming each
step. Report progress as a banner at the start, structured logs throughout,
and a final summary table when done.

## Pre-flight banner

Before any work, print this to stdout (verbatim format, fill in the values
yourself via Bash):

```
═══════════════════════════════════════════════════════════════
 ccc-init:lazy — full bootstrap   ($(date -u +%FT%TZ))
═══════════════════════════════════════════════════════════════
 pwd:           <output of `pwd`>
 user:          <$USER>
 host:          <$(hostname)>
 shell:         <$SHELL>
 os / arch:     <$(uname -sm)>
 git branch:    <output of `git branch --show-current`>
 git remote:    <output of `git remote get-url origin` or "(none)">
 git status:    <"clean" or N modified, N untracked>
 crabcc:        <crabcc --version  or  "NOT INSTALLED">
 ccc:           <ccc --version     or  "NOT INSTALLED">
 ollama:        <ollama --version  or  "NOT INSTALLED">
 task / cargo:  <task --version, cargo --version short forms>
═══════════════════════════════════════════════════════════════
```

If `crabcc` or `ccc` is not installed, fail fast with a clear note: tell
the user to `cd <crabcc-source> && cargo install --path crates/crabcc-cli`
and stop. Do NOT try to install crabcc itself from this command.

## Logging contract

All step output (commands run + their stdout/stderr) is captured into
`.crabcc/init.log` in the current repo. Use `tee` so the user also sees it
live. Pattern for each step:

```bash
mkdir -p .crabcc
LOG=".crabcc/init.log"
echo "" >> "$LOG"
echo "─── [$(date -u +%T)] STEP <N>: <name> ───" | tee -a "$LOG"
<command> 2>&1 | tee -a "$LOG"
```

If a step fails, log the failure, continue with the next step, and surface
all failures in the final summary table. Do not abort the whole flow on
the first error — the user wants the lazy "best-effort end-to-end" pass.

## Steps (run in order)

### 1. crabcc index — full repo index

```bash
crabcc index
```

Capture stats from the JSON output (`files`, `symbols`, `signatures`).

### 2. graph build — populate `.crabcc/graph.json`

```bash
crabcc graph build
```

### 3. memory init — drawer store + WAL

```bash
crabcc memory init
crabcc memory health
```

### 3a. memory mining — project source + Claude Code sessions

Populate the drawer store with real content so semantic search has signal
from the first query. Both miners are idempotent (sha-keyed) and safe to
re-run.

```bash
# Project files: walks the indexed source tree, drops a drawer per file.
ccc memory mine project .  2>&1 | tee -a "$LOG" || true

# Claude Code transcripts: one drawer per session (last 30 days by default).
# Skip silently if the projects dir doesn't exist (user not on Claude Code).
if [ -d "$HOME/.claude/projects" ]; then
    ccc memory mine sessions "$HOME/.claude/projects" 2>&1 | tee -a "$LOG" || true
fi
```

The `--features memory-embed` build path lazily downloads the MiniLM-L6-v2
ONNX model on first mine (~25 MB into `~/.cache/crabcc-memory/`). If the
binary was built without that feature, mining still works — drawers land
without vectors and lexical/BM25 search covers them.

### 4. install aliases + missing modern tools

```bash
bash <crabcc-source>/scripts/install-aliases.sh --aggressive --all-shells --install-tools
```

`--install-tools` invokes `brew install …` (macOS) or `apt-get install …`
(Linux) for whichever of `rg fd bat eza dust duf procs btop zoxide jq
git-delta` are missing. If the user lacks brew/apt, log the gap and move
on; do NOT prompt.

If `<crabcc-source>` is unknown, locate it via `which crabcc` →
`readlink -f`/`realpath` → walk up to the cargo workspace root. Fall back
to `~/.cargo/git/checkouts/crabcc-*` or just skip this step with a note.

### 5. update check + upgrade

```bash
crabcc upgrade --check
```

If the JSON shows `available: true`, run:

```bash
crabcc upgrade
```

If `gh` auth is missing, log the gap and skip the upgrade — don't prompt.

### 6. live view — background

```bash
nohup crabcc serve --port 0 --no-open > .crabcc/serve.log 2>&1 &
echo $! > .crabcc/serve.pid
```

Wait 2 seconds, grep `.crabcc/serve.log` for the bound port, then print
the URL `http://127.0.0.1:<port>/live` to stdout. Tell the user the panel
shows live tool calls + the slowly-growing relation graph.

### 6a. watchdog — `crabcc watch` keeps the index warm

Auto-refresh the index whenever files change on disk so the live view
and the ollama walkthrough see real-time edits. Background process,
PID tracked.

```bash
nohup crabcc watch > .crabcc/watch.log 2>&1 &
echo $! > .crabcc/watch.pid
```

Default debounce (500ms) is fine for human-edit cadences. The user can
tune via `crabcc watch --debounce <ms>` after the bootstrap.

### 6b. guard agent — lifecycle supervisor

The ollama walkthrough (step 7), `crabcc serve`, and `crabcc watch` are
all long-running children. Spawn a small **guard agent** that watches
their PIDs, kills the ollama process at the 10-minute hard timeout (or
sooner if it goes idle), reaps zombies, and logs each lifecycle event
to `.crabcc/guard.log`. The guard exits cleanly when ollama finishes;
serve + watch keep running until the user stops them.

```bash
cat > .crabcc/guard.sh <<'GUARD'
#!/usr/bin/env bash
# Auto-generated by /ccc-init:lazy — supervisor for ollama / serve / watch.
set -uo pipefail
LOG=".crabcc/guard.log"
log() { echo "[$(date -u +%T)] $*" | tee -a "$LOG"; }

OLLAMA_DEADLINE=$(( $(date +%s) + 600 ))   # 10-min hard cap
while true; do
    sleep 5
    # Reap zombies inside our own pgroup. (`wait -n` would block on the
    # ollama child; we want a non-blocking sweep.)
    while kill -0 0 2>/dev/null; do break; done

    # Hard timeout on ollama.
    if [ -f .crabcc/ollama.pid ]; then
        OPID=$(cat .crabcc/ollama.pid)
        if kill -0 "$OPID" 2>/dev/null; then
            if [ "$(date +%s)" -gt "$OLLAMA_DEADLINE" ]; then
                log "ollama (pid $OPID) exceeded 600s deadline — SIGTERM"
                kill -TERM "$OPID" 2>/dev/null
                sleep 5
                kill -KILL "$OPID" 2>/dev/null
                rm -f .crabcc/ollama.pid
                break
            fi
        else
            log "ollama (pid $OPID) exited"
            rm -f .crabcc/ollama.pid
            break
        fi
    fi

    # Heartbeat for serve + watch — restart if they died unexpectedly
    # in the first 30s of the bootstrap (covers transient port races).
    for svc in serve watch; do
        pf=".crabcc/$svc.pid"
        [ -f "$pf" ] || continue
        spid=$(cat "$pf")
        if ! kill -0 "$spid" 2>/dev/null; then
            log "$svc (pid $spid) died — restarting"
            nohup crabcc "$svc" > ".crabcc/$svc.log" 2>&1 &
            echo $! > "$pf"
        fi
    done
done
log "guard exiting cleanly"
GUARD
chmod +x .crabcc/guard.sh
nohup bash .crabcc/guard.sh > /dev/null 2>&1 &
echo $! > .crabcc/guard.pid
```

**Definition: zombie process.** A `<defunct>` entry in `ps` whose parent
hasn't reaped its exit status. We avoid them by either (a) waiting on
each child explicitly, or (b) double-forking so `init`/PID-1 reaps. The
guard takes path (a) for ollama and lets the OS reap serve + watch when
the user terminates the session.

### 7. ollama walkthrough — knowledge-graph warm-up (10-min timeout)

This is the long-running, "lazy" step the user invoked the command for.
Spawn an ollama agent that exercises crabcc's surface against the indexed
repo to populate the graph + memory drawers with real query traces.
Runs as a child with PID recorded so the guard agent can supervise it.

**Preference order (issue #105):**

1. If the bundled Ollama auth stack is up (`crabcc ollama-stack status`
   returns at least one healthy container), prefer
   `crabcc agent --backend ollama --run "<prompt>"` — that path goes
   through the LiteLLM proxy with proper Bearer-auth and benefits from
   the auto-up check + correlated tracing in `~/.crabcc/agents/`.
2. If no stack but `crabcc install-claude --with-ollama-stack` is
   available, optionally bring it up first.
3. Fall back to the local `ollama` binary path below for users who
   want zero-Docker.

```bash
if crabcc ollama-stack status 2>/dev/null | jq -e 'length > 0' >/dev/null; then
    # Path 1 — stack is up, use crabcc agent so the run is observable + correlated.
    crabcc agent --backend ollama --run "$(cat <<'PROMPT'
Exercise the indexed repo via `crabcc` / `ccc` calls. Print each command,
then its output. Goal: ~5–10 symbols across 3+ files, drop notes into
memory drawers as you go. End in ≤ 10 minutes. Do NOT mutate source.
PROMPT
)" 2>&1 | tee -a "$LOG" &
    echo $! > .crabcc/ollama.pid
    wait $(cat .crabcc/ollama.pid) || true
elif command -v ollama >/dev/null 2>&1; then
    # Path 3 — local ollama binary (legacy path).
    ( timeout 600 ollama run llama3.2:latest <<'PROMPT' 2>&1 | tee -a "$LOG" ) &
    echo $! > .crabcc/ollama.pid
    wait $(cat .crabcc/ollama.pid) || true
You are exploring an unfamiliar codebase using the `crabcc` and `ccc`
CLI tools. The user has just indexed the repo at the current working
directory. Spend the next ~10 minutes building a knowledge graph by
running the following kinds of probes (one at a time; print the
command, then the output):

- `ccc list --files | head -40` — what's here
- `ccc list --orphans` — entry points / public surface
- `ccc list --cycles` — mutual recursion hot spots
- `crabcc outline <interesting-file>` — top-level structure
- `ccc find <symbol>` for any symbol that looks load-bearing
- `ccc find <symbol> --mode references --files-only` — fan-out
- `ccc find <symbol> --mode callers --files-only` — fan-in
- `crabcc memory remember "doc:<short-id>" "<one-paragraph note>"` —
  drop your own observations into the memory store as you go

Do NOT modify source files. Do NOT run package managers, network
commands, or anything that mutates state outside `.crabcc/`. End when
you've looked at 5–10 distinct symbols across 3+ files.
PROMPT
else
    echo "ollama not installed — skipping walkthrough" | tee -a "$LOG"
fi
```

Adjust the model name (`llama3.2:latest`) to whatever the user has
pulled — fall back to `ollama list | head -2 | tail -1 | awk '{print $1}'`
if `llama3.2:latest` isn't available.

### 8. write `.ccc.crabcc` marker — locations + IDs + reload command

The marker proves the bootstrap ran and serves as the **single source of
truth** for "where did /ccc-init:lazy put things and how do I reload?".
Format is `key: value` (line-oriented, grep-friendly) with three sections:
**versions / IDs / locations**, **PIDs**, and **reload commands**.

Generate a UUID for this init session (use `uuidgen` if available, else
`/proc/sys/kernel/random/uuid`, else a sha256 of `pwd + epoch`). Capture
the Claude Code session id if `$CLAUDE_SESSION_ID` is set in the env
(it's exported by Claude Code for hook scripts).

```bash
INIT_UUID=$(uuidgen 2>/dev/null \
            || cat /proc/sys/kernel/random/uuid 2>/dev/null \
            || echo "$(pwd):$(date +%s)" | shasum -a 256 | cut -c1-36)
LIVE_PORT=$(grep -oE 'listening on [^ ]+:[0-9]+' .crabcc/serve.log 2>/dev/null \
            | tail -1 | awk -F: '{print $NF}')

cat > .ccc.crabcc <<EOF
# crabcc + ccc init marker — auto-generated by /ccc-init:lazy
# DO NOT EDIT BY HAND. Re-run \`/ccc-init:lazy\` to refresh.

# === versions + identity ============================================
init_at:            $(date -u +%FT%TZ)
init_uuid:          ${INIT_UUID}
crabcc_version:     $(crabcc --version 2>/dev/null | awk '{print \$2}')
ccc_version:        $(ccc --version 2>/dev/null | awk '{print \$2}')
ollama_version:     $(ollama --version 2>/dev/null | awk '{print \$NF}')
host:               $(hostname)
user:               $USER
shell:              $SHELL
os_arch:            $(uname -sm)
claude_session_id:  ${CLAUDE_SESSION_ID:-(unset)}
git_remote:         $(git remote get-url origin 2>/dev/null || echo "(none)")
git_branch:         $(git branch --show-current 2>/dev/null || echo "(detached)")
git_commit:         $(git rev-parse HEAD 2>/dev/null || echo "(no git)")

# === workspace state on disk ========================================
init_pwd:           $(pwd)
crabcc_dir:         $(pwd)/.crabcc
index_db:           $(pwd)/.crabcc/index.db
memory_db:          $(pwd)/.crabcc/memory.db
graph_json:         $(pwd)/.crabcc/graph.json
fts_dir:            $(pwd)/.crabcc/tantivy
fsst_symbols:       $(pwd)/.crabcc/fsst.symbols
init_log:           $(pwd)/.crabcc/init.log
serve_log:          $(pwd)/.crabcc/serve.log
watch_log:          $(pwd)/.crabcc/watch.log
guard_log:          $(pwd)/.crabcc/guard.log
ollama_history:     $(pwd)/.crabcc/init.log    # walkthrough output is teed here
fastembed_cache:    $HOME/.cache/crabcc-memory  # MiniLM ONNX cache
claude_projects:    $HOME/.claude/projects      # mined into memory drawers

# === background PIDs (kill any with: kill \$(cat .crabcc/<svc>.pid)) ===
serve_pid:          $(cat .crabcc/serve.pid 2>/dev/null || echo none)
watch_pid:          $(cat .crabcc/watch.pid 2>/dev/null || echo none)
guard_pid:          $(cat .crabcc/guard.pid 2>/dev/null || echo none)
ollama_pid:         $(cat .crabcc/ollama.pid 2>/dev/null || echo none)
live_view_url:      http://127.0.0.1:${LIVE_PORT:-?}/live

# === how to reload this context in a fresh shell ===================
# 1. Pick up the ccc-aware aliases:
#       source ~/.zshrc
# 2. Status snapshot:
#       ccc info                       # see crabcc info --status-line
# 3. Replay knowledge from the walkthrough:
#       ccc memory list --limit 50
#       ccc memory search "<query>"    # hybrid (BM25 + cosine RRF)
# 4. Re-open the live dashboard (only if guard is still alive):
#       open http://127.0.0.1:${LIVE_PORT:-?}/live
# 5. Stop background services:
#       kill \$(cat .crabcc/serve.pid .crabcc/watch.pid .crabcc/guard.pid)
# 6. If \`ccc\` isn't recognised, re-run install-aliases:
#       bash <crabcc-source>/scripts/install-aliases.sh --aggressive --all-shells
EOF
```

Add `.ccc.crabcc` to `.gitignore` (idempotently — only if not already there).
The marker IS local-only state; never commit it.

### 9. final summary

Print a one-line-per-step status table + the live-view URL + the load-context
reload one-liner. Example:

```
─── ccc-init:lazy summary ───
✓ index            files=287  symbols=4231
✓ graph            edges=8923 cycles=3 orphans=11
✓ memory           drawers=0 (will grow during walkthrough)
✓ aliases          shell=zsh+bash, aggressive=on, missing-tools=0
✓ upgrade-check    up-to-date (v2.6.0)
✓ live-view        http://127.0.0.1:54321/live
✓ ollama           28 probes, 14 drawers written, ~7m elapsed
✓ marker           .ccc.crabcc written; .gitignore updated

Reload in fresh shell:
  source ~/.zshrc && ccc info && ccc memory list --limit 50
```

## Constraints / behaviour

- **Idempotent.** Re-running on an already-init'd repo refreshes everything
  without breaking anything. `crabcc index` over an existing index is fine;
  the alias block is fenced; `.ccc.crabcc` is overwritten.
- **No prompts.** The user said "lazy" — execute end-to-end.
- **Best-effort.** Skip steps whose tooling is missing (ollama, brew, gh)
  and surface them in the summary. Don't abort the run.
- **Background processes.** The `crabcc serve` PID lives at
  `.crabcc/serve.pid`. Tell the user once at the end how to stop it
  (`kill $(cat .crabcc/serve.pid)`).
- **Logs.** Everything mirrors to `.crabcc/init.log`. Tell the user the
  log path in the final summary so they can re-read.
