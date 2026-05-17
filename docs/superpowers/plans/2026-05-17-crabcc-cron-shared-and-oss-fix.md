# crabcc-cron — shared layer + WL-2 OSS-fix dispatcher — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the shared cron-runner contract + the first workload (WL-2 OSS-fix dispatcher) per `docs/superpowers/specs/2026-05-17-crabcc-cron-shared-and-oss-fix-design.md`. End state: every 4h on Hetzner, the dispatcher picks one eligible upstream issue, fires opencode+deepseek-v4-pro at it in an isolated clone, opens a draft PR if tests pass, and writes one finding per tick to a Chroma cloud collection.

**Architecture:** Pure bash scripts under `tools/crabcc-cron/`. Workloads emit JSONL on stdout; `crabcc-cron-emit` filters `kind=="finding"` and POSTs to Chroma. Config is TOML (parsed via `python3 -c 'import tomllib...'` shim — no new dependency on the target box beyond what ships with Python 3.11+). Cron entries in `/etc/cron.d/crabcc-cron` on the Hetzner deploy.

**Tech Stack:** bash 5, jq, curl, python3 stdlib (`tomllib`), gh CLI, opencode CLI, bats-core (test runner), shellcheck (lint). No Rust crates added. No new runtime dependencies beyond what's already on a default Hetzner Ubuntu install + gh + opencode.

**Out of scope for this plan** (each gets its own follow-up spec):
- Spool retry on Chroma failure (Section 3.5 of spec). MVP fails the emit, logs a `warn`, and lets the next cron tick's natural retry-shape handle eventual consistency. Findings missed during a Chroma outage are accepted as a small known gap.
- Tier1 auto-discovery via `gh repo list peterlodri-sec`. MVP ships with `tier2_curated` only.
- Multi-language test command detection. MVP supports `cargo test --workspace` for Rust only. Non-Rust upstreams in the config are skipped with a `no_fix` finding.
- Per-attempt gist of `opencode.log`. MVP inlines the last 50 lines of opencode output into the PR body in a `<details>` block.

---

## File Structure

```
tools/crabcc-cron/
├── README.md                          # operator notes (deploy + smoke)
├── bin/
│   ├── crabcc-cron-emit               # stdin JSONL → Chroma POST (bash)
│   └── crabcc-cron-config-shim        # python3 tomllib → shell vars
├── jobs/
│   └── oss-fix.sh                     # WL-2 dispatcher entrypoint
├── lib/
│   ├── log.sh                         # JSONL log/finding helpers
│   ├── upstream.sh                    # config → working set
│   ├── eligibility.sh                 # issue eligibility predicate
│   ├── sandbox.sh                     # /srv/cron-agents/oss-fix/* setup
│   ├── agent.sh                       # opencode run wrapper + outcome
│   └── pr.sh                          # gh pr create flow + caps
├── templates/
│   └── oss-fix.md                     # opencode prompt template
├── deploy/
│   ├── install.sh                     # Hetzner installer (idempotent)
│   ├── crabcc-cron.cron               # /etc/cron.d/crabcc-cron template
│   └── env.example                    # /etc/crabcc-cron/env template
└── tests/
    ├── helpers.bash                   # shared bats helpers
    ├── unit/
    │   ├── emit_validation.bats
    │   ├── emit_id_hashing.bats
    │   ├── emit_metadata_flatten.bats
    │   ├── emit_chroma_post.bats
    │   ├── upstream_working_set.bats
    │   ├── eligibility_predicate.bats
    │   ├── outcome_parsing.bats
    │   └── caps.bats
    └── e2e/
        └── oss_fix_dryrun.sh          # follows scripts/tests/e2e/_runner.e2e.sh
```

---

## Phase A — Shared layer

### Task A1: Scaffold + bats wiring

**Files:**
- Create: `tools/crabcc-cron/README.md`
- Create: `tools/crabcc-cron/tests/helpers.bash`
- Create: `tools/crabcc-cron/.shellcheckrc`
- Modify: `Taskfile.yml` — add `cron-test` and `cron-lint` targets

- [ ] **Step 1: Write the README skeleton**

```markdown
# crabcc-cron

Cron-driven workload runner. See
`docs/superpowers/specs/2026-05-17-crabcc-cron-shared-and-oss-fix-design.md`
for the design.

## Layout

- `bin/` — shared utilities invoked from every workload
- `jobs/` — workload entrypoints (one per cron entry)
- `lib/` — sourced bash helpers (workload-shared logic)
- `templates/` — prompt templates for agent invocations
- `deploy/` — installer + cron + env templates for the target box
- `tests/` — bats unit tests + e2e smoke

## Local development

```bash
# Lint
task cron-lint

# Run unit tests
task cron-test

# Run e2e smoke (requires gh + opencode in PATH)
OSS_FIX_DRY_RUN=1 bash tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
```

## Deployment

See `deploy/install.sh` and `deploy/README.md`. Production target is a
Hetzner box at `/opt/crabcc-cron/`.
```

- [ ] **Step 2: Write `tests/helpers.bash`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/tests/helpers.bash — shared bats helpers.

# Always called from the repo root.
CRON_ROOT="${BATS_TEST_DIRNAME}/.."
export CRON_ROOT

# A scratch tempdir per test, auto-cleaned.
setup_tempdir() {
  TMPD="$(mktemp -d)"
  export TMPD
}

teardown_tempdir() {
  rm -rf "$TMPD"
}

# Run a script under test, capture stdout/stderr/status.
# Usage: run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"finding"...}'
run_script() {
  local script="$1"; shift
  STDOUT="$("${CRON_ROOT}/${script}" "$@" 2>"${TMPD:?}/stderr")"
  STATUS=$?
  STDERR="$(cat "${TMPD}/stderr")"
  export STDOUT STDERR STATUS
}

# Assert STDOUT contains substring.
assert_stdout_contains() {
  [[ "$STDOUT" == *"$1"* ]] || {
    echo "expected STDOUT to contain: $1"
    echo "got: $STDOUT"
    return 1
  }
}

assert_stderr_contains() {
  [[ "$STDERR" == *"$1"* ]] || {
    echo "expected STDERR to contain: $1"
    echo "got: $STDERR"
    return 1
  }
}

assert_status_eq() {
  [[ "$STATUS" -eq "$1" ]] || {
    echo "expected status $1, got $STATUS"
    return 1
  }
}
```

- [ ] **Step 3: Write `tools/crabcc-cron/.shellcheckrc`**

```
# Bash 5 target; allow source-without-extension.
shell=bash
external-sources=true
```

- [ ] **Step 4: Add Taskfile targets**

Open `Taskfile.yml`. Add at the end of the `tasks:` block:

```yaml
  cron-lint:
    desc: shellcheck all crabcc-cron scripts
    cmds:
      - shellcheck -x tools/crabcc-cron/bin/* tools/crabcc-cron/jobs/* tools/crabcc-cron/lib/* tools/crabcc-cron/deploy/install.sh

  cron-test:
    desc: bats unit tests for crabcc-cron
    cmds:
      - bats tools/crabcc-cron/tests/unit
```

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/README.md \
        tools/crabcc-cron/tests/helpers.bash \
        tools/crabcc-cron/.shellcheckrc \
        Taskfile.yml
git commit -m "feat(crabcc-cron): scaffold + bats/shellcheck wiring"
```

---

### Task A2: `crabcc-cron-emit` — schema validation + id hashing

**Files:**
- Create: `tools/crabcc-cron/bin/crabcc-cron-emit`
- Create: `tools/crabcc-cron/tests/unit/emit_validation.bats`
- Create: `tools/crabcc-cron/tests/unit/emit_id_hashing.bats`

- [ ] **Step 1: Write the failing tests**

`tools/crabcc-cron/tests/unit/emit_validation.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "emit: ignores log lines, only emits findings" {
  run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"log","level":"info","msg":"hi"}'
  assert_status_eq 0
  [[ -z "$STDOUT" ]] || { echo "expected empty stdout"; return 1; }
}

@test "emit: rejects finding missing required field 'workload'" {
  run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"finding","severity":"info","title":"t","body":"b"}'
  assert_status_eq 2
  assert_stderr_contains "missing required field: workload"
}

@test "emit: rejects finding with unknown severity" {
  payload='{"kind":"finding","workload":"x","severity":"meh","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  assert_status_eq 2
  assert_stderr_contains "invalid severity"
}

@test "emit: ignores non-JSON lines with a warning" {
  run_script bin/crabcc-cron-emit --dry-run <<<'not-json'
  assert_status_eq 0
  assert_stderr_contains "skipped non-JSON line"
}

@test "emit: ignores unknown 'kind' values" {
  run_script bin/crabcc-cron-emit --dry-run <<<'{"kind":"unknown","msg":"x"}'
  assert_status_eq 0
}
```

`tools/crabcc-cron/tests/unit/emit_id_hashing.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "emit: computes id from workload+repo+title when id absent" {
  payload='{"kind":"finding","workload":"oss-fix","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  assert_status_eq 0
  # Dry-run prints the would-be POST body. Assert id field is a 64-char hex string.
  echo "$STDOUT" | jq -e '.id | test("^[a-f0-9]{64}$")' >/dev/null
}

@test "emit: preserves caller-supplied id" {
  payload='{"kind":"finding","id":"my-explicit-id","workload":"oss-fix","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -e '.id == "my-explicit-id"' >/dev/null
}

@test "emit: same input → same id (deterministic)" {
  payload='{"kind":"finding","workload":"oss-fix","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  ID1="$(echo "$STDOUT" | jq -r '.id')"
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  ID2="$(echo "$STDOUT" | jq -r '.id')"
  [[ "$ID1" == "$ID2" ]] || { echo "ids differ: $ID1 vs $ID2"; return 1; }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `bats tools/crabcc-cron/tests/unit/emit_validation.bats tools/crabcc-cron/tests/unit/emit_id_hashing.bats`
Expected: all 8 tests fail with `crabcc-cron-emit: no such file`.

- [ ] **Step 3: Write `bin/crabcc-cron-emit`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/bin/crabcc-cron-emit
#
# Reads JSONL on stdin. For lines with kind=="finding", validates the
# schema, fills in id if missing (sha256 of canonical key), and either
# prints the would-be POST body (--dry-run) or POSTs to Chroma.
#
# All other lines (logs, malformed JSON, unknown kinds) are dropped with
# a warning to stderr. Always exits 0 on per-line errors so cron pipes
# don't abort upstream workloads. Exits 2 only on hard schema errors
# in --dry-run mode (so unit tests can assert them).
#
# Required env (non-dry-run): CHROMA_HOST, CHROMA_TENANT, CHROMA_DATABASE,
# CHROMA_API_KEY.

set -uo pipefail

DRY_RUN=0
case "${1-}" in
  --dry-run) DRY_RUN=1; shift ;;
esac

REQUIRED_FIELDS=(workload severity title body)
VALID_SEVERITIES=(info warn error)

emit_err() { echo "crabcc-cron-emit: $*" >&2; }

# sha256 hex of stdin. Uses sha256sum on Linux, shasum on macOS.
sha256_hex() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

process_finding() {
  local line="$1"
  # Validate JSON
  if ! jq -e '.' >/dev/null 2>&1 <<<"$line"; then
    emit_err "skipped non-JSON line"
    return 0
  fi
  local kind; kind="$(jq -r '.kind // empty' <<<"$line")"
  [[ "$kind" == "finding" ]] || return 0

  # Required fields
  local f
  for f in "${REQUIRED_FIELDS[@]}"; do
    if [[ "$(jq -r ".${f} // empty" <<<"$line")" == "" ]]; then
      emit_err "missing required field: $f"
      [[ $DRY_RUN -eq 1 ]] && exit 2
      return 0
    fi
  done

  # Severity check
  local sev; sev="$(jq -r '.severity' <<<"$line")"
  local ok=0
  for s in "${VALID_SEVERITIES[@]}"; do [[ "$s" == "$sev" ]] && ok=1; done
  if [[ $ok -eq 0 ]]; then
    emit_err "invalid severity: $sev"
    [[ $DRY_RUN -eq 1 ]] && exit 2
    return 0
  fi

  # Compute id if missing
  local id; id="$(jq -r '.id // empty' <<<"$line")"
  if [[ -z "$id" ]]; then
    local key
    key="$(jq -r '"\(.workload):\(.repo // ""):\(.title)"' <<<"$line")"
    id="$(printf '%s' "$key" | sha256_hex)"
  fi

  # Build POST body (metadata flatten happens in A3)
  local body
  body="$(jq --arg id "$id" '. + {id: $id}' <<<"$line")"

  if [[ $DRY_RUN -eq 1 ]]; then
    echo "$body"
  else
    # POST to Chroma (implemented in A4)
    chroma_post "$body"
  fi
}

# Stub; real impl in Task A4.
chroma_post() { :; }

main() {
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "$line" ]] && continue
    process_finding "$line"
  done
}

main "$@"
```

- [ ] **Step 4: Make it executable and run tests**

```bash
chmod +x tools/crabcc-cron/bin/crabcc-cron-emit
bats tools/crabcc-cron/tests/unit/emit_validation.bats \
     tools/crabcc-cron/tests/unit/emit_id_hashing.bats
```

Expected: all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/bin/crabcc-cron-emit \
        tools/crabcc-cron/tests/unit/emit_validation.bats \
        tools/crabcc-cron/tests/unit/emit_id_hashing.bats
git commit -m "feat(crabcc-cron): emit script with schema validation + id hashing"
```

---

### Task A3: `crabcc-cron-emit` — metadata flattening + body truncation

**Files:**
- Modify: `tools/crabcc-cron/bin/crabcc-cron-emit` (extend `process_finding`)
- Create: `tools/crabcc-cron/tests/unit/emit_metadata_flatten.bats`

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "emit: flattens metadata with meta. prefix" {
  payload='{"kind":"finding","workload":"x","repo":"a/b","severity":"info","title":"t","body":"b","metadata":{"pr_number":42,"branch":"foo"}}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  assert_status_eq 0
  echo "$STDOUT" | jq -e '."meta.pr_number" == 42' >/dev/null
  echo "$STDOUT" | jq -e '."meta.branch" == "foo"' >/dev/null
}

@test "emit: drops the nested 'metadata' object after flattening" {
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b","metadata":{"k":"v"}}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -e '.metadata // false | not' >/dev/null
}

@test "emit: truncates body over 8 KiB and adds [truncated] marker" {
  long_body=$(head -c 9000 /dev/urandom | base64 | head -c 9000)
  payload="$(jq -nc --arg b "$long_body" '{kind:"finding",workload:"x",severity:"info",title:"t",body:$b}')"
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -r '.body' | tail -c 20 | grep -q '\[truncated\]'
  bytes=$(echo "$STDOUT" | jq -r '.body' | wc -c)
  [[ "$bytes" -le 8200 ]]  # 8192 + small slack for marker
}

@test "emit: stamps ts (ISO 8601 with timezone)" {
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit --dry-run <<<"$payload"
  echo "$STDOUT" | jq -e '.ts | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T")' >/dev/null
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `bats tools/crabcc-cron/tests/unit/emit_metadata_flatten.bats`
Expected: 4 fails (metadata not flattened, body not truncated, no ts).

- [ ] **Step 3: Extend `process_finding`**

In `tools/crabcc-cron/bin/crabcc-cron-emit`, replace the body-building block (the line `body="$(jq --arg id "$id" '. + {id: $id}' <<<"$line")"`) with:

```bash
  # Truncate body if > 8 KiB
  local b; b="$(jq -r '.body' <<<"$line")"
  local b_bytes; b_bytes=$(printf '%s' "$b" | wc -c)
  if (( b_bytes > 8192 )); then
    b="$(printf '%s' "$b" | head -c 8000)... [truncated]"
  fi

  # Timestamp (ISO 8601 with timezone)
  local ts; ts="$(date -Iseconds 2>/dev/null || date '+%Y-%m-%dT%H:%M:%S%z')"

  # Flatten metadata with meta. prefix
  body="$(jq --arg id "$id" --arg ts "$ts" --arg body "$b" '
    . as $orig
    | (.metadata // {}) as $m
    | (reduce ($m | to_entries[]) as $kv ({}; .["meta." + $kv.key] = $kv.value)) as $flat
    | $orig + $flat + {id: $id, ts: $ts, body: $body} | del(.metadata) | del(.kind)
  ' <<<"$line")"
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/emit_metadata_flatten.bats
```

Expected: 4 passes.

Re-run prior unit tests to ensure no regression:

```bash
bats tools/crabcc-cron/tests/unit
```

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/bin/crabcc-cron-emit \
        tools/crabcc-cron/tests/unit/emit_metadata_flatten.bats
git commit -m "feat(crabcc-cron): emit flattens metadata, truncates body, stamps ts"
```

---

### Task A4: `crabcc-cron-emit` — Chroma POST

**Files:**
- Modify: `tools/crabcc-cron/bin/crabcc-cron-emit` (replace `chroma_post` stub)
- Create: `tools/crabcc-cron/tests/unit/emit_chroma_post.bats`

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # Fake curl that records the request and returns the env-controlled
  # response. Prepended to PATH.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/curl" <<'EOF'
#!/usr/bin/env bash
# Capture all args + stdin for assertion.
echo "$@" >"$FAKE_CURL_ARGS"
cat >"$FAKE_CURL_BODY"
# Honor FAKE_CURL_STATUS (default 200) and FAKE_CURL_OUT (default empty).
echo "${FAKE_CURL_OUT:-}"
exit "${FAKE_CURL_EXIT:-0}"
EOF
  chmod +x "$TMPD/bin/curl"
  export PATH="$TMPD/bin:$PATH"
  export FAKE_CURL_ARGS="$TMPD/curl.args"
  export FAKE_CURL_BODY="$TMPD/curl.body"
  export CHROMA_HOST="https://api.trychroma.cloud"
  export CHROMA_TENANT="t"
  export CHROMA_DATABASE="d"
  export CHROMA_API_KEY="secret"
}
teardown() { teardown_tempdir; }

@test "emit: POSTs to Chroma upsert endpoint with API key header" {
  payload='{"kind":"finding","workload":"x","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  assert_status_eq 0
  grep -q 'api.trychroma.cloud' "$FAKE_CURL_ARGS"
  grep -q 'X-Chroma-Token: secret' "$FAKE_CURL_ARGS"
  grep -q 'cron-findings' "$FAKE_CURL_ARGS"
}

@test "emit: POST body has documents/metadatas/ids arrays" {
  payload='{"kind":"finding","workload":"x","repo":"a/b","severity":"info","title":"t","body":"the body","metadata":{"k":"v"}}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  jq -e '.documents | length == 1' "$FAKE_CURL_BODY"
  jq -e '.documents[0] == "the body"' "$FAKE_CURL_BODY"
  jq -e '.ids | length == 1' "$FAKE_CURL_BODY"
  jq -e '.metadatas[0].workload == "x"' "$FAKE_CURL_BODY"
  jq -e '.metadatas[0]."meta.k" == "v"' "$FAKE_CURL_BODY"
}

@test "emit: non-2xx response is logged but does not abort" {
  export FAKE_CURL_OUT='{"error":"internal"}'
  export FAKE_CURL_STATUS=500
  # We use --write-out in the real script to capture status; the fake
  # curl echoes whatever we tell it, and the script appends -w '%{http_code}'
  # → tests inject status via FAKE_CURL_OUT prefix.
  export FAKE_CURL_OUT='{"error":"internal"}500'
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  assert_status_eq 0
  assert_stderr_contains "chroma POST failed"
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `bats tools/crabcc-cron/tests/unit/emit_chroma_post.bats`
Expected: 3 fails (chroma_post is a no-op).

- [ ] **Step 3: Replace the `chroma_post` stub**

In `tools/crabcc-cron/bin/crabcc-cron-emit`, replace `chroma_post() { :; }` with:

```bash
chroma_post() {
  local body="$1"

  # Build Chroma payload: one document at a time.
  local doc id meta payload
  doc="$(jq -r '.body' <<<"$body")"
  id="$(jq -r '.id' <<<"$body")"
  meta="$(jq '. | del(.body) | del(.id)' <<<"$body")"

  payload="$(jq -nc \
    --arg id "$id" \
    --arg doc "$doc" \
    --argjson meta "$meta" \
    '{ids: [$id], documents: [$doc], metadatas: [$meta]}')"

  # Chroma cloud v2 upsert endpoint.
  local url="${CHROMA_HOST}/api/v2/tenants/${CHROMA_TENANT}/databases/${CHROMA_DATABASE}/collections/cron-findings/upsert"

  local response status
  response="$(curl -sS -o - \
    -w '%{http_code}' \
    -X POST \
    -H "Content-Type: application/json" \
    -H "X-Chroma-Token: ${CHROMA_API_KEY}" \
    --data "$payload" \
    "$url")"
  status="${response: -3}"
  if [[ ! "$status" =~ ^2[0-9][0-9]$ ]]; then
    emit_err "chroma POST failed (status=$status, response=${response%$status})"
  fi
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/emit_chroma_post.bats
bats tools/crabcc-cron/tests/unit  # full sweep
```

Expected: all unit tests pass (11 total at this point).

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/bin/crabcc-cron-emit \
        tools/crabcc-cron/tests/unit/emit_chroma_post.bats
git commit -m "feat(crabcc-cron): emit POSTs to Chroma cloud upsert endpoint"
```

---

### Task A5: Hetzner installer + crontab template + env example

**Files:**
- Create: `tools/crabcc-cron/deploy/install.sh`
- Create: `tools/crabcc-cron/deploy/crabcc-cron.cron`
- Create: `tools/crabcc-cron/deploy/env.example`
- Create: `tools/crabcc-cron/deploy/README.md`

- [ ] **Step 1: Write the cron template**

`tools/crabcc-cron/deploy/crabcc-cron.cron`:

```
# Installed to /etc/cron.d/crabcc-cron — do not edit in place.
# Edit tools/crabcc-cron/deploy/crabcc-cron.cron in the repo and re-run
# tools/crabcc-cron/deploy/install.sh.

SHELL=/bin/bash
BASH_ENV=/etc/crabcc-cron/env
MAILTO=""

# WL-2 OSS-fix dispatcher — every 4h.
0 */4 * * * deploy  /opt/crabcc-cron/jobs/oss-fix.sh 2>&1 | tee >(systemd-cat -t crabcc-cron-oss-fix) | /opt/crabcc-cron/bin/crabcc-cron-emit
```

- [ ] **Step 2: Write the env example**

`tools/crabcc-cron/deploy/env.example`:

```bash
# /etc/crabcc-cron/env — chmod 600 root:deploy, sourced by every cron entry.

# GitHub — peterlodri-sec account
export GH_TOKEN=ghp_REDACTED

# Anthropic (for opencode if it falls back)
export ANTHROPIC_API_KEY=sk-REDACTED

# Opencode model selection
export OPENCODE_MODEL=deepseek-v4-pro
export OPENCODE_API_KEY=sk-REDACTED

# Opencode runtime tuning (steady-state)
export OSS_FIX_MAX_TOKENS=200000
export OSS_FIX_TIMEOUT=30m

# Chroma cloud
export CHROMA_HOST=https://api.trychroma.cloud
export CHROMA_TENANT=REDACTED
export CHROMA_DATABASE=cron
export CHROMA_API_KEY=ck-REDACTED

# Dry-run gate. Set to 1 during the first week of deployment.
export OSS_FIX_DRY_RUN=1
```

- [ ] **Step 3: Write the installer**

`tools/crabcc-cron/deploy/install.sh`:

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/deploy/install.sh — idempotent install onto Hetzner.
#
# Run as root on the target box (e.g. via ssh deploy@hetzner sudo bash).
# Symlinks /opt/crabcc-cron/ to a checkout under /srv/repos/crabcc/tools/crabcc-cron,
# installs /etc/cron.d/crabcc-cron, and creates /etc/crabcc-cron/env from
# env.example if it doesn't exist (operator must edit secrets in-place).

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-/srv/repos/crabcc}"
SRC="${REPO_ROOT}/tools/crabcc-cron"
DEST="/opt/crabcc-cron"
ETC="/etc/crabcc-cron"
CRON="/etc/cron.d/crabcc-cron"

if [[ "$EUID" -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

# 1. Symlink /opt/crabcc-cron → repo checkout (so `git pull` updates the deploy).
if [[ -L "$DEST" || -e "$DEST" ]]; then
  rm -rf "$DEST"
fi
ln -s "$SRC" "$DEST"

# 2. /etc/crabcc-cron exists with env file. Don't overwrite if present.
mkdir -p "$ETC"
chmod 750 "$ETC"
chown root:deploy "$ETC"
if [[ ! -f "$ETC/env" ]]; then
  cp "$SRC/deploy/env.example" "$ETC/env"
  chmod 600 "$ETC/env"
  chown root:deploy "$ETC/env"
  echo "Created $ETC/env from env.example — edit secrets before running cron." >&2
fi

# 3. Install crontab.
cp "$SRC/deploy/crabcc-cron.cron" "$CRON"
chmod 644 "$CRON"
chown root:root "$CRON"

# 4. State + spool dirs.
mkdir -p /opt/crabcc-cron-state /opt/crabcc-cron-state/oss-fix
mkdir -p /srv/cron-agents/oss-fix
chown -R deploy:deploy /opt/crabcc-cron-state /srv/cron-agents

# 5. Smoke: shellcheck pass (catches typos before cron picks it up).
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -x "$SRC/bin/"* "$SRC/jobs/"* "$SRC/lib/"* || {
    echo "shellcheck failed; aborting" >&2
    exit 1
  }
fi

echo "crabcc-cron installed. Next cron tick will run."
```

- [ ] **Step 4: Write the deploy README**

`tools/crabcc-cron/deploy/README.md`:

```markdown
# Deploying crabcc-cron to Hetzner

```bash
# On the box:
sudo apt-get install -y jq curl gh shellcheck python3
# Install opencode per its own docs.

# Clone the repo (deploy user):
sudo mkdir -p /srv/repos && sudo chown deploy:deploy /srv/repos
git clone https://github.com/peterlodri-sec/crabcc.git /srv/repos/crabcc

# Install (as root):
sudo bash /srv/repos/crabcc/tools/crabcc-cron/deploy/install.sh

# Edit secrets:
sudo -u deploy vi /etc/crabcc-cron/env

# Watch first run:
sudo journalctl -t crabcc-cron-oss-fix -f
```

## Updating

```bash
cd /srv/repos/crabcc && git pull
sudo bash tools/crabcc-cron/deploy/install.sh  # re-runs idempotently
```

## Disabling

```bash
sudo rm /etc/cron.d/crabcc-cron
```
```

- [ ] **Step 5: Lint and commit**

```bash
shellcheck -x tools/crabcc-cron/deploy/install.sh
task cron-test
task cron-lint
git add tools/crabcc-cron/deploy/
git commit -m "feat(crabcc-cron): hetzner installer + crontab + env template"
```

---

## Phase B — WL-2 OSS-fix dispatcher

### Task B1: Config parser shim (TOML → shell)

**Files:**
- Create: `tools/crabcc-cron/bin/crabcc-cron-config-shim`
- Create: `tools/crabcc-cron/tests/unit/config_shim.bats`
- Create: `tools/crabcc-cron/deploy/oss-fix.toml.example`

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/config_shim.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "config-shim: emits shell var for tier2 include list" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b", "c/d"]

[tier3_deny]
exclude = []
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_status_eq 0
  assert_stdout_contains 'TIER2_INCLUDE=( "a/b" "c/d" )'
}

@test "config-shim: emits shell var for tier3 deny list" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b"]

[tier3_deny]
exclude = ["x/y", "z/w"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'TIER3_EXCLUDE=( "x/y" "z/w" )'
}

@test "config-shim: missing file → exit 2 with clear error" {
  run_script bin/crabcc-cron-config-shim "$TMPD/nonexistent.toml"
  assert_status_eq 2
  assert_stderr_contains "config file not found"
}

@test "config-shim: missing tier2_curated section → empty include" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier3_deny]
exclude = []
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'TIER2_INCLUDE=()'
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/config_shim.bats
```

Expected: 4 fails (script doesn't exist).

- [ ] **Step 3: Write the shim**

`tools/crabcc-cron/bin/crabcc-cron-config-shim`:

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/bin/crabcc-cron-config-shim
#
# Reads a TOML config file and emits shell variable declarations on stdout.
# Caller does: eval "$(crabcc-cron-config-shim /etc/crabcc-cron/oss-fix.toml)"
#
# Uses python3 stdlib tomllib (Python 3.11+, default on Ubuntu 22.04+).

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: crabcc-cron-config-shim <file.toml>" >&2
  exit 2
fi

if [[ ! -f "$1" ]]; then
  echo "config file not found: $1" >&2
  exit 2
fi

python3 - "$1" <<'PY'
import sys, tomllib

with open(sys.argv[1], "rb") as f:
    cfg = tomllib.load(f)

def emit_array(name, items):
    if not items:
        print(f"{name}=()")
        return
    quoted = " ".join(f'"{x}"' for x in items)
    print(f"{name}=( {quoted} )")

emit_array("TIER2_INCLUDE", cfg.get("tier2_curated", {}).get("include", []))
emit_array("TIER3_EXCLUDE", cfg.get("tier3_deny", {}).get("exclude", []))
PY
```

- [ ] **Step 4: Write the example config**

`tools/crabcc-cron/deploy/oss-fix.toml.example`:

```toml
# /etc/crabcc-cron/oss-fix.toml — WL-2 dispatcher config.

[tier2_curated]
include = [
  "rust-lang/cargo",
  "tokio-rs/tokio",
  "tree-sitter/tree-sitter",
  "rusqlite/rusqlite",
]

[tier3_deny]
# Override: never touch these.
exclude = []
```

- [ ] **Step 5: Make executable, run tests, commit**

```bash
chmod +x tools/crabcc-cron/bin/crabcc-cron-config-shim
bats tools/crabcc-cron/tests/unit/config_shim.bats
git add tools/crabcc-cron/bin/crabcc-cron-config-shim \
        tools/crabcc-cron/tests/unit/config_shim.bats \
        tools/crabcc-cron/deploy/oss-fix.toml.example
git commit -m "feat(crabcc-cron): toml config shim for oss-fix dispatcher"
```

---

### Task B2: Eligibility predicate

**Files:**
- Create: `tools/crabcc-cron/lib/eligibility.sh`
- Create: `tools/crabcc-cron/tests/unit/eligibility_predicate.bats`

The predicate is a pure function over an issue JSON record (the shape returned by `gh issue view --json ...`). Tested in isolation.

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/eligibility_predicate.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # Source the lib so `issue_is_eligible` is in scope.
  # NOW=ts overrides current-time for deterministic age math.
  export NOW="2026-05-17T00:00:00Z"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/eligibility.sh"
}
teardown() { teardown_tempdir; }

# helpers: build an issue JSON object with overrides.
mk_issue() {
  jq -nc \
    --arg created "${1:-2026-04-01T00:00:00Z}" \
    --arg updated "${2:-2026-04-01T00:00:00Z}" \
    --argjson assignees "${3:-[]}" \
    --argjson linkedBranches "${4:-[]}" \
    --argjson commentsTotal "${5:-0}" \
    '{createdAt:$created, updatedAt:$updated, assignees:$assignees, linkedBranches:$linkedBranches, comments:{totalCount:$commentsTotal}}'
}

@test "eligibility: passes for fresh, unassigned, untouched, low-comment issue" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" "[]" "[]" 3)
  run issue_is_eligible "$issue"
  [[ "$status" -eq 0 ]]
}

@test "eligibility: rejects assigned issue" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" '[{"login":"x"}]' "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue with linked branch (existing PR)" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" "[]" '[{"name":"fix-x"}]' 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue younger than 7d" {
  # 5 days before NOW (2026-05-17)
  issue=$(mk_issue "2026-05-12T00:00:00Z" "2026-05-12T00:00:00Z" "[]" "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue older than 180d" {
  issue=$(mk_issue "2025-11-01T00:00:00Z" "2025-11-01T00:00:00Z" "[]" "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue actively discussed (updated < 30d ago)" {
  # Created 60d ago but updated 15d ago.
  issue=$(mk_issue "2026-03-15T00:00:00Z" "2026-05-02T00:00:00Z" "[]" "[]" 0)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}

@test "eligibility: rejects issue with > 10 comments" {
  issue=$(mk_issue "2026-04-01T00:00:00Z" "2026-04-01T00:00:00Z" "[]" "[]" 12)
  run issue_is_eligible "$issue"
  [[ "$status" -ne 0 ]]
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/eligibility_predicate.bats
```

Expected: 7 fails (lib doesn't exist).

- [ ] **Step 3: Write the predicate**

`tools/crabcc-cron/lib/eligibility.sh`:

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/eligibility.sh
#
# Pure-function predicate over an issue JSON record.
# Returns 0 iff the issue is eligible per spec §4.3.
#
# Honors $NOW (ISO 8601 string) for deterministic testing; defaults to
# current time.

# Convert ISO 8601 string to unix seconds.
_iso_to_epoch() {
  if [[ "$(uname)" == "Darwin" ]]; then
    date -j -f "%Y-%m-%dT%H:%M:%SZ" "$1" +%s 2>/dev/null
  else
    date -d "$1" +%s 2>/dev/null
  fi
}

_now_epoch() {
  if [[ -n "${NOW:-}" ]]; then
    _iso_to_epoch "$NOW"
  else
    date +%s
  fi
}

issue_is_eligible() {
  local issue="$1"
  local now created updated age_days idle_days assignees linked comments_total

  now="$(_now_epoch)"
  created="$(_iso_to_epoch "$(jq -r '.createdAt' <<<"$issue")")"
  updated="$(_iso_to_epoch "$(jq -r '.updatedAt' <<<"$issue")")"
  age_days=$(( (now - created) / 86400 ))
  idle_days=$(( (now - updated) / 86400 ))
  assignees="$(jq '.assignees | length' <<<"$issue")"
  linked="$(jq '.linkedBranches | length' <<<"$issue")"
  comments_total="$(jq '.comments.totalCount' <<<"$issue")"

  # All gates must pass.
  (( assignees == 0 ))      || return 1
  (( linked == 0 ))         || return 2
  (( age_days >= 7 ))       || return 3
  (( age_days <= 180 ))     || return 4
  (( idle_days >= 30 ))     || return 5
  (( comments_total <= 10 ))|| return 6
  return 0
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/eligibility_predicate.bats
```

Expected: 7 passes.

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/lib/eligibility.sh \
        tools/crabcc-cron/tests/unit/eligibility_predicate.bats
git commit -m "feat(crabcc-cron): issue eligibility predicate for oss-fix"
```

---

### Task B3: Upstream working-set computation

**Files:**
- Create: `tools/crabcc-cron/lib/upstream.sh`
- Create: `tools/crabcc-cron/tests/unit/upstream_working_set.bats`

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/upstream.sh"
}
teardown() { teardown_tempdir; }

@test "upstream: working set = tier2 minus tier3" {
  TIER2_INCLUDE=( "a/b" "c/d" "e/f" )
  TIER3_EXCLUDE=( "c/d" )
  result=$(upstream_working_set)
  [[ "$result" == "a/b"$'\n'"e/f" ]]
}

@test "upstream: empty tier3 leaves tier2 unchanged" {
  TIER2_INCLUDE=( "a/b" "c/d" )
  TIER3_EXCLUDE=()
  result=$(upstream_working_set)
  [[ "$result" == "a/b"$'\n'"c/d" ]]
}

@test "upstream: tier3 covering all of tier2 yields empty set" {
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=( "a/b" )
  result=$(upstream_working_set)
  [[ -z "$result" ]]
}

@test "upstream: empty tier2 yields empty set" {
  TIER2_INCLUDE=()
  TIER3_EXCLUDE=( "a/b" )
  result=$(upstream_working_set)
  [[ -z "$result" ]]
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/upstream_working_set.bats
```

Expected: 4 fails.

- [ ] **Step 3: Write `lib/upstream.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/upstream.sh
#
# Computes the working set of upstreams for this tick.
# Reads TIER2_INCLUDE[] and TIER3_EXCLUDE[] from the environment
# (set by `eval "$(crabcc-cron-config-shim ...)"`).
#
# Tier1 auto-discovery via gh repo list is deferred to a follow-up plan.

upstream_working_set() {
  local r excluded
  for r in "${TIER2_INCLUDE[@]:-}"; do
    [[ -z "$r" ]] && continue
    excluded=0
    for ex in "${TIER3_EXCLUDE[@]:-}"; do
      [[ "$ex" == "$r" ]] && { excluded=1; break; }
    done
    (( excluded == 0 )) && printf '%s\n' "$r"
  done
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/upstream_working_set.bats
```

Expected: 4 passes.

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/lib/upstream.sh \
        tools/crabcc-cron/tests/unit/upstream_working_set.bats
git commit -m "feat(crabcc-cron): upstream working-set (tier2 minus tier3)"
```

---

### Task B4: Issue picker (per-upstream gh query + eligibility + tie-break)

**Files:**
- Create: `tools/crabcc-cron/lib/picker.sh`
- Create: `tools/crabcc-cron/tests/unit/picker.bats`

The picker iterates upstreams, calls `gh issue list` for each, applies the eligibility predicate, and returns the chosen issue (or empty). `gh` is mocked via fake-PATH in tests.

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  export NOW="2026-05-17T00:00:00Z"
  # Fake gh: returns fixture-based JSON from $TMPD/fixtures.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
# Args: issue list --repo <r> --label ... --state open --json ...
# OR:   repo view <r> --json stargazerCount
# Routes based on first two args.
case "$1 $2" in
  "issue list")
    # find --repo flag value
    while [[ $# -gt 0 ]]; do
      case "$1" in --repo) shift; repo="$1" ;; esac
      shift
    done
    cat "$TMPD/fixtures/issues-${repo//\//--}.json" 2>/dev/null || echo "[]"
    ;;
  "repo view")
    repo="$3"
    cat "$TMPD/fixtures/repo-${repo//\//--}.json" 2>/dev/null || echo '{"stargazerCount":0}'
    ;;
esac
EOF
  chmod +x "$TMPD/bin/gh"
  export PATH="$TMPD/bin:$PATH"
  mkdir -p "$TMPD/fixtures"
  export OSS_FIX_STATE_DIR="$TMPD/state"
  mkdir -p "$OSS_FIX_STATE_DIR"

  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/eligibility.sh"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/picker.sh"
}
teardown() { teardown_tempdir; }

@test "picker: returns null when no upstreams" {
  result=$(pick_issue)
  [[ "$result" == "" ]]
}

@test "picker: returns null when all upstreams have no eligible issues" {
  echo '[]' >"$TMPD/fixtures/issues-a--b.json"
  echo '{"stargazerCount":100}' >"$TMPD/fixtures/repo-a--b.json"
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  [[ "$result" == "" ]]
}

@test "picker: returns the eligible issue from the only upstream" {
  cat >"$TMPD/fixtures/issues-a--b.json" <<EOF
[{"number":42,"title":"t","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":3}}]
EOF
  echo '{"stargazerCount":50}' >"$TMPD/fixtures/repo-a--b.json"
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  echo "$result" | jq -e '.repo == "a/b"' >/dev/null
  echo "$result" | jq -e '.issue.number == 42' >/dev/null
}

@test "picker: prefers upstream with higher star count" {
  cat >"$TMPD/fixtures/issues-a--b.json" <<EOF
[{"number":10,"title":"low","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":1}}]
EOF
  echo '{"stargazerCount":10}' >"$TMPD/fixtures/repo-a--b.json"
  cat >"$TMPD/fixtures/issues-c--d.json" <<EOF
[{"number":99,"title":"high","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":1}}]
EOF
  echo '{"stargazerCount":1000}' >"$TMPD/fixtures/repo-c--d.json"
  TIER2_INCLUDE=( "a/b" "c/d" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  echo "$result" | jq -e '.repo == "c/d"' >/dev/null
  echo "$result" | jq -e '.issue.number == 99' >/dev/null
}

@test "picker: skips issues already attempted (state file present)" {
  cat >"$TMPD/fixtures/issues-a--b.json" <<EOF
[{"number":42,"title":"t","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":3}}]
EOF
  echo '{"stargazerCount":50}' >"$TMPD/fixtures/repo-a--b.json"
  touch "$OSS_FIX_STATE_DIR/a--b--42.attempted"
  TIER2_INCLUDE=( "a/b" )
  TIER3_EXCLUDE=()
  result=$(pick_issue)
  [[ "$result" == "" ]]
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/picker.bats
```

Expected: 5 fails.

- [ ] **Step 3: Write `lib/picker.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/picker.sh
#
# Picks one eligible issue across the upstream working set.
# Strategy: across all upstreams, pick the lowest issue number on the
# upstream with the highest star count. Ties broken alphabetically.
#
# Depends on: lib/upstream.sh (in caller scope), lib/eligibility.sh.
# Honors $OSS_FIX_STATE_DIR for per-issue state files
# (key: <owner>--<repo>--<issue>.attempted).

# Print JSON object {repo:"owner/name", issue:{...}, stars: N} on stdout,
# or empty string if no eligible issue exists.
pick_issue() {
  local r issues issue n stars best_stars="" best_payload=""
  while IFS= read -r r; do
    [[ -z "$r" ]] && continue
    issues="$(gh issue list \
      --repo "$r" \
      --label "good first issue,help wanted,E-easy,D-easy" \
      --state open \
      --json number,title,labels,assignees,linkedBranches,createdAt,updatedAt,comments 2>/dev/null \
      || echo '[]')"
    stars="$(gh repo view "$r" --json stargazerCount 2>/dev/null | jq -r '.stargazerCount // 0')"

    # Filter to eligible issues, sort ascending by number, take first.
    while IFS= read -r issue; do
      [[ -z "$issue" ]] && continue
      n="$(jq -r '.number' <<<"$issue")"
      [[ -f "${OSS_FIX_STATE_DIR}/${r//\//--}--${n}.attempted" ]] && continue
      if issue_is_eligible "$issue"; then
        if [[ -z "$best_stars" || "$stars" -gt "$best_stars" ]]; then
          best_stars="$stars"
          best_payload="$(jq -nc --arg repo "$r" --argjson stars "$stars" --argjson issue "$issue" \
            '{repo:$repo, stars:$stars, issue:$issue}')"
          # Only need the lowest-numbered eligible issue per repo, hence break.
        fi
        break
      fi
    done < <(jq -c 'sort_by(.number) | .[]' <<<"$issues")
  done < <(upstream_working_set)

  [[ -n "$best_payload" ]] && printf '%s' "$best_payload"
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/picker.bats
```

Expected: 5 passes.

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/lib/picker.sh \
        tools/crabcc-cron/tests/unit/picker.bats
git commit -m "feat(crabcc-cron): issue picker (eligibility + star-weighted)"
```

---

### Task B5: Sandbox setup + agent invocation + outcome parsing

**Files:**
- Create: `tools/crabcc-cron/lib/sandbox.sh`
- Create: `tools/crabcc-cron/lib/agent.sh`
- Create: `tools/crabcc-cron/templates/oss-fix.md`
- Create: `tools/crabcc-cron/tests/unit/agent_outcome.bats`

This task combines three small concerns into one cohesive unit because they share the same data flow (the sandbox dir) and the outcome parsing is trivial.

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/agent_outcome.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/agent.sh"
}
teardown() { teardown_tempdir; }

@test "outcome: STATUS=fixed last line → 'fixed'" {
  cat >"$TMPD/log" <<'EOF'
some output
STATUS=fixed
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "fixed" ]]
}

@test "outcome: STATUS=tests-failed → 'tests-failed'" {
  cat >"$TMPD/log" <<'EOF'
STATUS=tests-failed
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "tests-failed" ]]
}

@test "outcome: STATUS=no-fix → 'no-fix'" {
  cat >"$TMPD/log" <<'EOF'
STATUS=no-fix the issue is actually a design discussion
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "no-fix" ]]
}

@test "outcome: SIGTERM exit (124) + no STATUS line → 'timeout'" {
  cat >"$TMPD/log" <<'EOF'
some incomplete work
EOF
  result=$(parse_outcome "$TMPD/log" 124)
  [[ "$result" == "timeout" ]]
}

@test "outcome: non-zero exit + no STATUS line → 'error'" {
  cat >"$TMPD/log" <<'EOF'
crashed
EOF
  result=$(parse_outcome "$TMPD/log" 1)
  [[ "$result" == "error" ]]
}

@test "outcome: zero exit + no STATUS line → 'error'" {
  # Suspicious — agent exited clean without declaring outcome.
  cat >"$TMPD/log" <<'EOF'
quiet exit
EOF
  result=$(parse_outcome "$TMPD/log" 0)
  [[ "$result" == "error" ]]
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/agent_outcome.bats
```

Expected: 6 fails.

- [ ] **Step 3: Write `lib/sandbox.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/sandbox.sh
#
# Creates and cleans per-attempt sandbox dirs under
# /srv/cron-agents/oss-fix/<owner>--<repo>--<issue>/.

: "${SANDBOX_ROOT:=/srv/cron-agents/oss-fix}"

# Args: repo (owner/name), issue_number
# Echoes the absolute sandbox path; creates clone/ inside.
sandbox_create() {
  local repo="$1" issue="$2"
  local key="${repo//\//--}--${issue}"
  local dir="${SANDBOX_ROOT}/${key}"
  mkdir -p "$dir"
  echo "running" >"$dir/status"
  echo "$dir"
}

# Args: sandbox_dir, repo (owner/name), issue_number
# Clones the upstream into sandbox/clone and creates the working branch.
sandbox_clone() {
  local dir="$1" repo="$2" issue="$3"
  gh repo clone "$repo" "$dir/clone" -- --quiet
  git -C "$dir/clone" checkout -b "claude-cron/fix-${issue}"
}

# Args: sandbox_dir, final_status
sandbox_finalize() {
  local dir="$1" status="$2"
  echo "$status" >"$dir/status"
}
```

- [ ] **Step 4: Write `lib/agent.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/agent.sh
#
# Renders the prompt, invokes opencode, parses the outcome.

: "${OPENCODE_MODEL:=deepseek-v4-pro}"
: "${OSS_FIX_MAX_TOKENS:=200000}"
: "${OSS_FIX_TIMEOUT:=30m}"

# Args: template_path, repo, issue_json (whole jq record), test_cmd, out_prompt_path
agent_render_prompt() {
  local tpl="$1" repo="$2" issue="$3" test_cmd="$4" out="$5"
  local n title body
  n="$(jq -r '.number' <<<"$issue")"
  title="$(jq -r '.title' <<<"$issue")"
  body="$(jq -r '.body // ""' <<<"$issue")"
  sed -e "s|{N}|$n|g" \
      -e "s|{repo}|$repo|g" \
      -e "s|{title}|${title//|/\\|}|g" \
      -e "s|{test_cmd}|${test_cmd//|/\\|}|g" \
      "$tpl" \
    | awk -v body="$body" '/^Body:$/ {print; print body; next} {print}' \
    > "$out"
}

# Args: sandbox_dir, prompt_path
# Returns: exit code of opencode, captures stdout to sandbox/opencode.log
agent_run() {
  local dir="$1" prompt="$2"
  timeout "$OSS_FIX_TIMEOUT" opencode run \
    --model "$OPENCODE_MODEL" \
    --cwd "$dir/clone" \
    --prompt-file "$prompt" \
    --max-tokens "$OSS_FIX_MAX_TOKENS" \
    > "$dir/opencode.log" 2>&1
  return $?
}

# Args: log_path, exit_code
# Echoes one of: fixed | tests-failed | no-fix | timeout | error
parse_outcome() {
  local log="$1" exit_code="$2"
  local status_line
  status_line="$(grep -E '^STATUS=' "$log" 2>/dev/null | tail -1 || true)"
  if [[ -n "$status_line" ]]; then
    case "$status_line" in
      STATUS=fixed*)        echo "fixed" ;;
      STATUS=tests-failed*) echo "tests-failed" ;;
      STATUS=no-fix*)       echo "no-fix" ;;
      *)                    echo "error" ;;
    esac
    return
  fi
  # No STATUS line.
  if [[ "$exit_code" -eq 124 ]]; then
    echo "timeout"
  else
    echo "error"
  fi
}
```

- [ ] **Step 5: Write the prompt template, run tests, commit**

`tools/crabcc-cron/templates/oss-fix.md`:

```markdown
You are working on issue #{N} in {repo}: "{title}".

Body:

Repo root: . (you are already inside the working clone)
Branch:    claude-cron/fix-{N}

Task:
1. Read the issue. If unclear or actually a design discussion → STOP and
   write the literal string "STATUS=no-fix" on its own line followed by
   a one-paragraph reason.
2. Find the failing code/test, OR write a reproducing test if none
   exists.
3. Implement the minimal fix.
4. Run the test command for this repo: {test_cmd}. All must pass.
5. If green → commit. Don't push, don't open a PR (the wrapper does
   that). Final line of your output MUST be "STATUS=fixed".
6. If you can't make tests pass within budget → write "STATUS=tests-failed"
   followed by the diff you tried.
7. If you hit the timeout, the wrapper will mark "STATUS=timeout"
   automatically.

Hard rules:
- Single-file change preferred. Refuse multi-crate refactors.
- No new dependencies.
- Match existing code style; run any formatter the repo configures.
- No telemetry, debug prints, or commented-out code in the final diff.
```

Run tests + commit:

```bash
bats tools/crabcc-cron/tests/unit/agent_outcome.bats
shellcheck -x tools/crabcc-cron/lib/sandbox.sh tools/crabcc-cron/lib/agent.sh
git add tools/crabcc-cron/lib/sandbox.sh \
        tools/crabcc-cron/lib/agent.sh \
        tools/crabcc-cron/templates/oss-fix.md \
        tools/crabcc-cron/tests/unit/agent_outcome.bats
git commit -m "feat(crabcc-cron): sandbox + agent + outcome parsing"
```

---

### Task B6: Rate limit gates + PR opening

**Files:**
- Create: `tools/crabcc-cron/lib/pr.sh`
- Create: `tools/crabcc-cron/tests/unit/caps.bats`

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/caps.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  export OSS_FIX_STATE_DIR="$TMPD/state"
  mkdir -p "$OSS_FIX_STATE_DIR"
  # Fake gh that emits a controlled "open PR count" via env.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "pr list")
    # Honor FAKE_OPEN_PR_COUNT
    n="${FAKE_OPEN_PR_COUNT:-0}"
    # Emit n minimal records.
    seq 1 "$n" | jq -nc --argjson n "$n" '[range(0;$n) | {number: (.+1)}]'
    ;;
esac
EOF
  chmod +x "$TMPD/bin/gh"
  export PATH="$TMPD/bin:$PATH"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/pr.sh"
}
teardown() { teardown_tempdir; }

@test "caps: 0 open PRs → global_cap_ok=0 (under cap)" {
  export FAKE_OPEN_PR_COUNT=0
  run global_cap_reached
  [[ "$status" -ne 0 ]]
}

@test "caps: 2 open PRs → global_cap not reached" {
  export FAKE_OPEN_PR_COUNT=2
  run global_cap_reached
  [[ "$status" -ne 0 ]]
}

@test "caps: 3 open PRs → global_cap reached" {
  export FAKE_OPEN_PR_COUNT=3
  run global_cap_reached
  [[ "$status" -eq 0 ]]
}

@test "caps: per-upstream cap respected when last_pr file is fresh" {
  touch "$OSS_FIX_STATE_DIR/a--b.last_pr"
  run upstream_cap_reached "a/b"
  [[ "$status" -eq 0 ]]
}

@test "caps: per-upstream cap free when last_pr is > 7 days old" {
  touch -t 202604010000 "$OSS_FIX_STATE_DIR/a--b.last_pr"
  run upstream_cap_reached "a/b"
  [[ "$status" -ne 0 ]]
}

@test "caps: per-upstream cap free when no last_pr file" {
  run upstream_cap_reached "fresh/new"
  [[ "$status" -ne 0 ]]
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/caps.bats
```

Expected: 6 fails.

- [ ] **Step 3: Write `lib/pr.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/pr.sh
#
# Rate-limit gates and PR opening.

: "${OSS_FIX_STATE_DIR:=/opt/crabcc-cron-state/oss-fix}"
: "${OSS_FIX_GLOBAL_CAP:=3}"
: "${OSS_FIX_UPSTREAM_COOLDOWN_DAYS:=7}"

# Returns 0 iff the number of open agent-drafted PRs is >= cap.
global_cap_reached() {
  local count
  count=$(gh pr list \
    --author "@me" \
    --state open \
    --search 'in:body "automated agent"' \
    --json number 2>/dev/null | jq 'length')
  (( count >= OSS_FIX_GLOBAL_CAP ))
}

# Returns 0 iff the upstream has had a PR opened within
# OSS_FIX_UPSTREAM_COOLDOWN_DAYS.
upstream_cap_reached() {
  local repo="$1"
  local f="${OSS_FIX_STATE_DIR}/${repo//\//--}.last_pr"
  [[ -f "$f" ]] || return 1
  local mtime now age
  if [[ "$(uname)" == "Darwin" ]]; then
    mtime=$(stat -f %m "$f")
  else
    mtime=$(stat -c %Y "$f")
  fi
  now=$(date +%s)
  age=$(( (now - mtime) / 86400 ))
  (( age < OSS_FIX_UPSTREAM_COOLDOWN_DAYS ))
}

# Args: sandbox_dir, repo, issue_json
# Pushes the branch, opens a draft PR, returns PR URL on stdout, or empty on failure.
open_draft_pr() {
  local dir="$1" repo="$2" issue="$3"
  local n branch title log_tail body
  n="$(jq -r '.number' <<<"$issue")"
  branch="claude-cron/fix-${n}"
  title="$(jq -r '.title' <<<"$issue")"
  log_tail="$(tail -50 "$dir/opencode.log")"
  body="$(printf 'Closes #%s\n\n---\nThis PR was drafted by an automated agent (opencode + %s) running on cron. I will review and finalize before requesting merge.\n\n<details><summary>opencode.log (last 50 lines)</summary>\n\n```\n%s\n```\n\n</details>\n' \
    "$n" "${OPENCODE_MODEL:-deepseek-v4-pro}" "$log_tail")"

  (cd "$dir/clone" && git push origin "$branch")
  gh pr create \
    --repo "$repo" \
    --base main \
    --head "$branch" \
    --draft \
    --title "[draft] fix: $title" \
    --body "$body" 2>/dev/null
}

# Args: repo
# Touches the per-upstream cooldown file.
mark_upstream_pr() {
  local repo="$1"
  mkdir -p "$OSS_FIX_STATE_DIR"
  touch "${OSS_FIX_STATE_DIR}/${repo//\//--}.last_pr"
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/caps.bats
```

Expected: 6 passes.

- [ ] **Step 5: Commit**

```bash
shellcheck -x tools/crabcc-cron/lib/pr.sh
git add tools/crabcc-cron/lib/pr.sh \
        tools/crabcc-cron/tests/unit/caps.bats
git commit -m "feat(crabcc-cron): rate-limit gates + draft PR opening"
```

---

### Task B7: End-to-end dispatcher (`jobs/oss-fix.sh`) + log helpers

**Files:**
- Create: `tools/crabcc-cron/lib/log.sh`
- Create: `tools/crabcc-cron/jobs/oss-fix.sh`
- Create: `tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh`

This is the wiring task. The dispatcher composes everything we built. The e2e test runs in `OSS_FIX_DRY_RUN=1` mode end-to-end with mocked `gh` and a fake `opencode`.

- [ ] **Step 1: Write `lib/log.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/log.sh
#
# JSONL log + finding emitters. Workloads use these instead of bare `echo`.

log_line() {
  local level="$1"; shift
  jq -nc --arg level "$level" --arg msg "$*" '{kind:"log", level:$level, msg:$msg}'
}
log_info()  { log_line info  "$@"; }
log_warn()  { log_line warn  "$@"; }
log_error() { log_line error "$@"; }

# Args: severity, workload, repo, title, body, metadata_json
emit_finding() {
  local severity="$1" workload="$2" repo="$3" title="$4" body="$5" meta="${6:-{}}"
  jq -nc \
    --arg sev "$severity" \
    --arg wl  "$workload" \
    --arg repo "$repo" \
    --arg title "$title" \
    --arg body "$body" \
    --argjson meta "$meta" \
    '{kind:"finding", workload:$wl, repo:$repo, severity:$sev, title:$title, body:$body, metadata:$meta}'
}
```

- [ ] **Step 2: Write `jobs/oss-fix.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/jobs/oss-fix.sh — WL-2 dispatcher entrypoint.
#
# One tick = at most one PR attempt. Emits exactly one finding per tick.
#
# Honors:
#   $OSS_FIX_DRY_RUN — if 1, does everything except `git push` and `gh pr create`.

set -uo pipefail

CRON_ROOT="${CRON_ROOT:-/opt/crabcc-cron}"

# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/log.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/eligibility.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/upstream.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/picker.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/sandbox.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/agent.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/pr.sh"

: "${OSS_FIX_CONFIG:=/etc/crabcc-cron/oss-fix.toml}"
: "${OSS_FIX_STATE_DIR:=/opt/crabcc-cron-state/oss-fix}"
mkdir -p "$OSS_FIX_STATE_DIR"

# Load config into shell.
eval "$("${CRON_ROOT}/bin/crabcc-cron-config-shim" "$OSS_FIX_CONFIG")"

t_start=$(date +%s)

log_info "oss-fix tick start"

# Global cap.
if global_cap_reached; then
  log_info "global cap reached, skipping"
  emit_finding info oss-fix "" "at_cap" "Global cap of $OSS_FIX_GLOBAL_CAP open agent-drafted PRs reached." '{}'
  exit 0
fi

# Pick an issue.
pick="$(pick_issue)"
if [[ -z "$pick" ]]; then
  log_info "no eligible issue across upstreams"
  emit_finding info oss-fix "" "no_eligible_issue" "No upstream had an eligible issue this tick." '{}'
  exit 0
fi

repo="$(jq -r '.repo' <<<"$pick")"
issue="$(jq -c '.issue' <<<"$pick")"
n="$(jq -r '.number' <<<"$issue")"

# Per-upstream cap.
if upstream_cap_reached "$repo"; then
  log_info "upstream $repo in cooldown, skipping"
  emit_finding info oss-fix "$repo" "at_cap" "Per-upstream cap reached for $repo." \
    "$(jq -nc --argjson n "$n" '{issue_number:$n}')"
  exit 0
fi

# Sandbox + clone.
dir="$(sandbox_create "$repo" "$n")"
log_info "sandbox: $dir"
if ! sandbox_clone "$dir" "$repo" "$n" 2>>"$dir/opencode.log"; then
  log_error "clone failed"
  sandbox_finalize "$dir" "error"
  touch "${OSS_FIX_STATE_DIR}/${repo//\//--}--${n}.attempted"
  emit_finding error oss-fix "$repo" "clone_failed" \
    "Failed to clone $repo for issue #$n." \
    "$(jq -nc --argjson n "$n" '{issue_number:$n}')"
  exit 0
fi

# Render prompt.
test_cmd="cargo test --workspace"  # MVP: rust-only.
agent_render_prompt \
  "${CRON_ROOT}/templates/oss-fix.md" \
  "$repo" \
  "$issue" \
  "$test_cmd" \
  "$dir/prompt.md"

# Run agent.
log_info "invoking opencode for $repo#$n"
agent_run "$dir" "$dir/prompt.md"
ec=$?

# Parse outcome.
outcome="$(parse_outcome "$dir/opencode.log" "$ec")"
sandbox_finalize "$dir" "$outcome"
touch "${OSS_FIX_STATE_DIR}/${repo//\//--}--${n}.attempted"

duration=$(( $(date +%s) - t_start ))
meta="$(jq -nc --argjson n "$n" --argjson dur "$duration" --arg dir "$dir" --argjson ec "$ec" \
  '{issue_number:$n, attempt_dir:$dir, duration_s:$dur, opencode_exit_code:$ec}')"

case "$outcome" in
  fixed)
    if [[ "${OSS_FIX_DRY_RUN:-0}" -eq 1 ]]; then
      log_info "dry-run: would push branch + open draft PR for $repo#$n"
      emit_finding info oss-fix "$repo" "pr_opened_dryrun" \
        "Dry-run: would open draft PR for $repo#$n." "$meta"
    else
      pr_url="$(open_draft_pr "$dir" "$repo" "$issue" || true)"
      if [[ -n "$pr_url" ]]; then
        mark_upstream_pr "$repo"
        emit_finding info oss-fix "$repo" "pr_opened" \
          "Draft PR opened: $pr_url" \
          "$(jq -nc --argjson n "$n" --argjson dur "$duration" --arg url "$pr_url" \
              '{issue_number:$n, duration_s:$dur, pr_url:$url}')"
      else
        emit_finding error oss-fix "$repo" "pr_open_failed" \
          "Tests passed but PR creation failed." "$meta"
      fi
    fi
    ;;
  tests-failed)
    emit_finding warn oss-fix "$repo" "tests_failed" \
      "Agent attempted fix for $repo#$n; tests did not pass." "$meta"
    ;;
  no-fix)
    emit_finding info oss-fix "$repo" "no_fix" \
      "Agent declined to fix $repo#$n (unclear / out of scope)." "$meta"
    ;;
  timeout)
    emit_finding warn oss-fix "$repo" "timeout" \
      "Agent timed out on $repo#$n." "$meta"
    ;;
  *)
    emit_finding error oss-fix "$repo" "error" \
      "Agent crashed or exited without STATUS on $repo#$n." "$meta"
    ;;
esac

log_info "oss-fix tick done in ${duration}s (outcome=$outcome)"
exit 0
```

- [ ] **Step 3: Write the e2e smoke test**

`tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh`:

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
#
# End-to-end smoke for the OSS-fix dispatcher in DRY_RUN mode.
# Mocks: gh (returns canned issue), opencode (writes STATUS=fixed),
#        git+jq+date+curl+python3 are real.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd -P)"
TMPD="$(mktemp -d)"
trap 'rm -rf "$TMPD"' EXIT

# Build the fake bin dir.
mkdir -p "$TMPD/bin"

cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "issue list")
    cat <<'JSON'
[{"number":42,"title":"trivial fix","body":"do the thing","createdAt":"2026-04-01T00:00:00Z","updatedAt":"2026-04-01T00:00:00Z","assignees":[],"linkedBranches":[],"comments":{"totalCount":1}}]
JSON
    ;;
  "repo view") echo '{"stargazerCount":1000}' ;;
  "repo clone")
    # Create a minimal cargo project so cargo test will be a real path.
    mkdir -p "$3"
    cd "$3"
    git init --quiet
    git config user.email "bot@example.com"
    git config user.name  "bot"
    cat >Cargo.toml <<'C'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
C
    mkdir -p src; echo 'fn main(){}' >src/main.rs
    git add -A; git commit --quiet -m "init"
    ;;
  "pr list")  echo '[]' ;;
  "pr create") echo "https://github.com/example/repo/pull/9999" ;;
esac
EOF
chmod +x "$TMPD/bin/gh"

cat >"$TMPD/bin/opencode" <<'EOF'
#!/usr/bin/env bash
# opencode mock: writes STATUS=fixed and exits 0.
echo "STATUS=fixed"
exit 0
EOF
chmod +x "$TMPD/bin/opencode"

# Build the config.
mkdir -p "$TMPD/etc"
cat >"$TMPD/etc/oss-fix.toml" <<'TOML'
[tier2_curated]
include = ["example/repo"]
[tier3_deny]
exclude = []
TOML

# Build the state dir.
mkdir -p "$TMPD/state"

# Invoke.
PATH="$TMPD/bin:$PATH" \
CRON_ROOT="$ROOT" \
OSS_FIX_CONFIG="$TMPD/etc/oss-fix.toml" \
OSS_FIX_STATE_DIR="$TMPD/state" \
OSS_FIX_DRY_RUN=1 \
SANDBOX_ROOT="$TMPD/sandbox" \
NOW="2026-05-17T00:00:00Z" \
bash "$ROOT/jobs/oss-fix.sh" > "$TMPD/out.jsonl" 2> "$TMPD/err.log"

echo "--- stdout ---"
cat "$TMPD/out.jsonl"
echo "--- stderr ---"
cat "$TMPD/err.log"

# Assert exactly one finding and it has status=pr_opened_dryrun.
findings=$(grep -c '"kind":"finding"' "$TMPD/out.jsonl" || echo 0)
[[ "$findings" -eq 1 ]] || { echo "FAIL: expected 1 finding, got $findings"; exit 1; }
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e '.title == "pr_opened_dryrun"' >/dev/null || {
  echo "FAIL: expected title=pr_opened_dryrun"; exit 1;
}

echo "PASS: oss-fix dispatcher end-to-end dry-run"
```

- [ ] **Step 4: Run all tests**

```bash
chmod +x tools/crabcc-cron/jobs/oss-fix.sh \
         tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
task cron-test
task cron-lint
bash tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
```

Expected: all unit tests pass, e2e prints `PASS: oss-fix dispatcher end-to-end dry-run`.

- [ ] **Step 5: Commit**

```bash
git add tools/crabcc-cron/lib/log.sh \
        tools/crabcc-cron/jobs/oss-fix.sh \
        tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
git commit -m "feat(crabcc-cron): oss-fix dispatcher end-to-end + dry-run smoke"
```

---

## Final verification

After all 12 tasks are complete:

- [ ] **All unit + e2e tests green**

```bash
task cron-test
task cron-lint
bash tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh
```

- [ ] **Full repo tests still green** (sanity check that nothing else broke)

```bash
task ci
```

- [ ] **Spec coverage check** — verify every section of the spec maps to a task in this plan:

  | Spec section | Task |
  |---|---|
  | 3.2 Filesystem layout | A1, A5 |
  | 3.3 Workload contract | A2, A3, B7 (log helpers) |
  | 3.4 Chroma sink | A4 |
  | 3.5 Spool drainer | Deferred (out of scope) |
  | 3.6 Cron configuration | A5 |
  | 3.7 Secrets handling | A5 (env.example) |
  | 3.8 Observability | A5 (cron → systemd-cat → journalctl) |
  | 4.1 Cadence | A5 (cron line) |
  | 4.2 Upstream curation | B1 (config), B3 (working set). Tier1 deferred. |
  | 4.3 Issue selection | B2 (eligibility), B4 (picker) |
  | 4.4 Per-attempt sandbox | B5 (sandbox.sh) |
  | 4.5 Agent invocation | B5 (agent.sh, prompt template) |
  | 4.6 Outcome handling | B5 (parse_outcome), B7 (dispatch) |
  | 4.7 PR identity and body | B6 (pr.sh) |
  | 4.8 Rate limits | B6 (caps), B7 (state files) |
  | 4.9 Findings emitted | B7 (per-status branch in oss-fix.sh) |
  | 4.10 Failure modes | B7 (clone failure branch), B5 (timeout via SIGTERM) |
  | 5 Testing strategy | unit (bats) + e2e (oss_fix_dryrun.sh) per task |

- [ ] **Open follow-up plan stubs** for the three deferred items:
  - `docs/superpowers/plans/2026-05-17-crabcc-cron-spool-retry.md`
  - `docs/superpowers/plans/2026-05-17-crabcc-cron-tier1-autodiscover.md`
  - `docs/superpowers/plans/2026-05-17-crabcc-cron-multilang-test-detect.md`

  Each is one-line spec for now; flesh out when prioritized.

- [ ] **Open a PR to land the plan + implementation** from this branch
  (`claude/spec-crabcc-cron-2026-05-17`) into `main`.
