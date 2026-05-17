# crabcc-cron WL-3 — security research workload — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship WL-3 (security research workload) per `docs/superpowers/specs/2026-05-17-crabcc-cron-wl3-security-design.md`. End state: every night at 02:00 UTC on Hetzner, walk every Rust repo under `peterlodri-sec/*`, run `cargo audit`, and for each advisory emit one finding to the `cron-findings` Chroma collection with reverse-dep chain (via `cargo tree --invert`) and usage-site count (via `crabcc fuzzy`).

**Architecture:** Reuses the WL-2 shared layer 100% — `crabcc-cron-emit`, `lib/log.sh`, the Chroma `cron-findings` collection, and the bats test harness. Three new files (`jobs/security.sh` + two `lib/*.sh` helpers), one config-shim extension, one cron-line addition, one e2e smoke.

**Tech Stack:** bash 5, jq, `cargo audit` (Rust dep auditor), `cargo tree`, `crabcc` (this repo's own indexer), `gh` CLI, python3 stdlib `tomllib` (already used by the shim). No new system deps beyond `cargo install cargo-audit`.

**Out of scope** (per spec §8): GitHub-distributed indexes (own follow-up plan), GH issue creation, polyglot scanning, symbol-level reachability, parallel scans, suppression workflow.

---

## File Structure

```
tools/crabcc-cron/
├── bin/
│   └── crabcc-cron-config-shim       (modify — add SECURITY_DENY emit)
├── jobs/
│   └── security.sh                   (new — WL-3 entrypoint)
├── lib/
│   ├── audit_repos.sh                (new — repo enumeration)
│   └── audit_advisory.sh             (new — severity map + finding builder)
├── deploy/
│   ├── security.toml.example         (new — deny list template)
│   ├── crabcc-cron.cron              (modify — add 02:00 UTC entry)
│   └── install.sh                    (modify — preflight cargo-audit)
└── tests/
    ├── unit/
    │   ├── config_shim_security.bats (new — SECURITY_DENY emission)
    │   ├── audit_repos.bats          (new — enumeration)
    │   └── audit_advisory.bats       (new — severity + finding shape)
    └── e2e/
        └── security_smoke.sh         (new — full-path smoke)
```

---

## Task 1: Extend config-shim to emit `SECURITY_DENY`

**Rationale:** WL-3 needs its own deny list, read from a separate `/etc/crabcc-cron/security.toml`. The existing `crabcc-cron-config-shim` reads `[tier2_curated]` and `[tier3_deny]` for WL-2. Extend it to also recognize `[security_deny]`. The shim emits all three arrays unconditionally — empty when the stanza is absent. Each caller uses only the vars it needs.

**Files:**
- Modify: `tools/crabcc-cron/bin/crabcc-cron-config-shim` (extend the python heredoc)
- Create: `tools/crabcc-cron/tests/unit/config_shim_security.bats`

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/config_shim_security.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() { setup_tempdir; }
teardown() { teardown_tempdir; }

@test "config-shim: emits SECURITY_DENY array from [security_deny] exclude" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[security_deny]
exclude = ["scratch-repo", "old-fork"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_status_eq 0
  assert_stdout_contains 'SECURITY_DENY=( "scratch-repo" "old-fork" )'
}

@test "config-shim: missing [security_deny] → empty SECURITY_DENY=()" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'SECURITY_DENY=()'
}

@test "config-shim: empty [security_deny] exclude list → empty SECURITY_DENY=()" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[security_deny]
exclude = []
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'SECURITY_DENY=()'
}

@test "config-shim: all three stanzas emit their arrays" {
  cat >"$TMPD/cfg.toml" <<'EOF'
[tier2_curated]
include = ["a/b"]
[tier3_deny]
exclude = ["c/d"]
[security_deny]
exclude = ["e/f"]
EOF
  run_script bin/crabcc-cron-config-shim "$TMPD/cfg.toml"
  assert_stdout_contains 'TIER2_INCLUDE=( "a/b" )'
  assert_stdout_contains 'TIER3_EXCLUDE=( "c/d" )'
  assert_stdout_contains 'SECURITY_DENY=( "e/f" )'
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/config_shim_security.bats
```

Expected: 4 fails (`SECURITY_DENY=...` not emitted yet).

- [ ] **Step 3: Extend the shim**

Open `tools/crabcc-cron/bin/crabcc-cron-config-shim`. In the python heredoc, add one line after the existing `emit_array("TIER3_EXCLUDE", ...)` line:

```python
emit_array("SECURITY_DENY", cfg.get("security_deny", {}).get("exclude", []))
```

The final python block becomes:

```python
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
emit_array("SECURITY_DENY", cfg.get("security_deny", {}).get("exclude", []))
PY
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/config_shim_security.bats
bats tools/crabcc-cron/tests/unit/config_shim.bats   # existing — must still pass
```

Expected: 4/4 new pass; 4/4 prior pass (no regression).

- [ ] **Step 5: Commit and push**

```bash
git add tools/crabcc-cron/bin/crabcc-cron-config-shim \
        tools/crabcc-cron/tests/unit/config_shim_security.bats
git -c commit.gpgsign=false commit -m "feat(crabcc-cron): config-shim emits SECURITY_DENY for WL-3"
git push
```

---

## Task 2: `lib/audit_repos.sh` — repo enumeration

**Rationale:** Pure function that returns one repo name per line, given the shell array `SECURITY_DENY[]` and the GitHub state. Wraps `gh repo list` + `gh api /contents/Cargo.toml` filtering.

**Files:**
- Create: `tools/crabcc-cron/lib/audit_repos.sh`
- Create: `tools/crabcc-cron/tests/unit/audit_repos.bats`

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/audit_repos.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # Fake gh on PATH. Routes by first two args.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "repo list")
    cat "$TMPD/fixtures/repo-list.json" 2>/dev/null || echo '[]'
    ;;
  "api "*"/contents/Cargo.toml"*)
    # Path argument is something like /repos/peterlodri-sec/<name>/contents/Cargo.toml
    name=$(echo "$2" | sed 's|.*/peterlodri-sec/\([^/]*\)/.*|\1|')
    if [[ -f "$TMPD/fixtures/cargo-toml-${name}.exists" ]]; then
      echo '{"name":"Cargo.toml"}'
      exit 0
    else
      exit 1
    fi
    ;;
esac
EOF
  chmod +x "$TMPD/bin/gh"
  export PATH="$TMPD/bin:$PATH"
  mkdir -p "$TMPD/fixtures"
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/audit_repos.sh"
}
teardown() { teardown_tempdir; }

@test "audit_repos: returns Rust repos only (those with Cargo.toml)" {
  cat >"$TMPD/fixtures/repo-list.json" <<'EOF'
[{"name":"crabcc","defaultBranch":"main"},{"name":"docs-only","defaultBranch":"main"}]
EOF
  touch "$TMPD/fixtures/cargo-toml-crabcc.exists"
  # docs-only has no Cargo.toml marker
  SECURITY_DENY=()
  result=$(enumerate_audit_repos)
  [[ "$result" == "crabcc" ]]
}

@test "audit_repos: filters out repos in SECURITY_DENY" {
  cat >"$TMPD/fixtures/repo-list.json" <<'EOF'
[{"name":"crabcc","defaultBranch":"main"},{"name":"scratch","defaultBranch":"main"}]
EOF
  touch "$TMPD/fixtures/cargo-toml-crabcc.exists"
  touch "$TMPD/fixtures/cargo-toml-scratch.exists"
  SECURITY_DENY=("scratch")
  result=$(enumerate_audit_repos)
  [[ "$result" == "crabcc" ]]
}

@test "audit_repos: empty gh list returns empty" {
  echo '[]' >"$TMPD/fixtures/repo-list.json"
  SECURITY_DENY=()
  result=$(enumerate_audit_repos)
  [[ -z "$result" ]]
}

@test "audit_repos: all repos denied returns empty" {
  cat >"$TMPD/fixtures/repo-list.json" <<'EOF'
[{"name":"a","defaultBranch":"main"},{"name":"b","defaultBranch":"main"}]
EOF
  touch "$TMPD/fixtures/cargo-toml-a.exists"
  touch "$TMPD/fixtures/cargo-toml-b.exists"
  SECURITY_DENY=("a" "b")
  result=$(enumerate_audit_repos)
  [[ -z "$result" ]]
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/audit_repos.bats
```

Expected: 4 fails (`lib/audit_repos.sh` doesn't exist).

- [ ] **Step 3: Write `lib/audit_repos.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/audit_repos.sh
#
# Enumerates the working set of repos to audit. Reads SECURITY_DENY[]
# from the environment (set by `eval "$(crabcc-cron-config-shim ...)"`).
#
# Strategy:
#   1. `gh repo list peterlodri-sec --no-archived --limit 200`
#   2. For each, check if Cargo.toml exists at root via the GH API.
#   3. Drop anything in SECURITY_DENY[].
#
# Prints one repo name per line to stdout (just "<name>", not
# "peterlodri-sec/<name>"). Empty stdout means no eligible repos.

# Returns 0 always; caller checks stdout for emptiness.
enumerate_audit_repos() {
  local repos name denied ex
  repos="$(gh repo list peterlodri-sec \
    --no-archived \
    --limit 200 \
    --json name,defaultBranch 2>/dev/null \
    || echo '[]')"

  while IFS= read -r name; do
    [[ -z "$name" ]] && continue

    # Denylist check.
    denied=0
    for ex in "${SECURITY_DENY[@]:-}"; do
      [[ "$ex" == "$name" ]] && { denied=1; break; }
    done
    (( denied == 1 )) && continue

    # Rust repo check: does Cargo.toml exist at root?
    if gh api "/repos/peterlodri-sec/${name}/contents/Cargo.toml" >/dev/null 2>&1; then
      printf '%s\n' "$name"
    fi
  done < <(jq -r '.[].name' <<<"$repos")
  return 0
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/audit_repos.bats
shellcheck -x tools/crabcc-cron/lib/audit_repos.sh
```

Expected: 4/4 pass; shellcheck clean.

- [ ] **Step 5: Commit and push**

```bash
git add tools/crabcc-cron/lib/audit_repos.sh \
        tools/crabcc-cron/tests/unit/audit_repos.bats
git -c commit.gpgsign=false commit -m "feat(crabcc-cron): audit_repos enumeration for WL-3"
git push
```

---

## Task 3: `lib/audit_advisory.sh` — severity map + finding builder

**Rationale:** Two pure functions: `map_severity` (cargo-audit severity string → crabcc-cron severity), and `advisory_to_finding` (advisory JSON record + dep chain + usage count → JSONL finding). Tested in isolation.

**Files:**
- Create: `tools/crabcc-cron/lib/audit_advisory.sh`
- Create: `tools/crabcc-cron/tests/unit/audit_advisory.bats`

- [ ] **Step 1: Write the failing test**

`tools/crabcc-cron/tests/unit/audit_advisory.bats`:

```bash
#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # shellcheck disable=SC1091
  source "${CRON_ROOT}/lib/audit_advisory.sh"
}
teardown() { teardown_tempdir; }

# --- map_severity ---

@test "severity: critical → error" {
  result=$(map_severity "critical")
  [[ "$result" == "error" ]]
}

@test "severity: high → error" {
  result=$(map_severity "high")
  [[ "$result" == "error" ]]
}

@test "severity: medium → warn" {
  result=$(map_severity "medium")
  [[ "$result" == "warn" ]]
}

@test "severity: low → info" {
  result=$(map_severity "low")
  [[ "$result" == "info" ]]
}

@test "severity: informational → info" {
  result=$(map_severity "informational")
  [[ "$result" == "info" ]]
}

@test "severity: empty/missing → info" {
  result=$(map_severity "")
  [[ "$result" == "info" ]]
}

@test "severity: unknown value → info (safe default)" {
  result=$(map_severity "moderate-by-someone-else")
  [[ "$result" == "info" ]]
}

# --- advisory_to_finding ---

@test "advisory_to_finding: full happy path with dep chain and usage count" {
  advisory='{
    "advisory": {
      "id": "RUSTSEC-2024-0388",
      "title": "use-after-free in hashbrown",
      "description": "Affected versions of this crate are vulnerable to UAF.",
      "severity": "high",
      "cwe": ["CWE-416"]
    },
    "package": {
      "name": "hashbrown",
      "version": "0.14.5"
    },
    "versions": {
      "patched": [">=0.15.0"]
    }
  }'
  dep_chain="hashbrown 0.14.5\n└── indexmap 2.2.6"
  result=$(advisory_to_finding "crabcc" "$advisory" "$dep_chain" "12")
  # Validate it is a single-line JSON.
  echo "$result" | jq -e . >/dev/null
  echo "$result" | jq -e '.kind == "finding"' >/dev/null
  echo "$result" | jq -e '.workload == "security"' >/dev/null
  echo "$result" | jq -e '.repo == "crabcc"' >/dev/null
  echo "$result" | jq -e '.severity == "error"' >/dev/null
  echo "$result" | jq -e '.title | test("^RUSTSEC-2024-0388")' >/dev/null
  echo "$result" | jq -e '.metadata.advisory_id == "RUSTSEC-2024-0388"' >/dev/null
  echo "$result" | jq -e '.metadata.crate == "hashbrown"' >/dev/null
  echo "$result" | jq -e '.metadata.usage_site_count == 12' >/dev/null
  echo "$result" | jq -e '.metadata.cwe == "CWE-416"' >/dev/null
}

@test "advisory_to_finding: null usage_count produces metadata.usage_site_count == null" {
  advisory='{
    "advisory": {"id":"RUSTSEC-X","title":"t","description":"d","severity":"medium","cwe":[]},
    "package": {"name":"foo","version":"1.0.0"},
    "versions": {"patched":[">=2.0.0"]}
  }'
  result=$(advisory_to_finding "crabcc" "$advisory" "foo 1.0.0" "null")
  echo "$result" | jq -e '.metadata.usage_site_count == null' >/dev/null
  echo "$result" | jq -e '.metadata.usage_count_unavailable_reason | type == "string"' >/dev/null
}

@test "advisory_to_finding: empty cwe array → metadata.cwe == null" {
  advisory='{
    "advisory": {"id":"RUSTSEC-X","title":"t","description":"d","severity":"low","cwe":[]},
    "package": {"name":"foo","version":"1.0.0"},
    "versions": {"patched":[">=2.0.0"]}
  }'
  result=$(advisory_to_finding "crabcc" "$advisory" "foo 1.0.0" "0")
  echo "$result" | jq -e '.metadata.cwe == null' >/dev/null
}

@test "advisory_to_finding: severity mapped through map_severity" {
  advisory='{
    "advisory": {"id":"RUSTSEC-X","title":"t","description":"d","severity":"critical","cwe":[]},
    "package": {"name":"foo","version":"1.0.0"},
    "versions": {"patched":[">=2.0.0"]}
  }'
  result=$(advisory_to_finding "crabcc" "$advisory" "foo 1.0.0" "5")
  echo "$result" | jq -e '.severity == "error"' >/dev/null
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
bats tools/crabcc-cron/tests/unit/audit_advisory.bats
```

Expected: 11 fails.

- [ ] **Step 3: Write `lib/audit_advisory.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/lib/audit_advisory.sh
#
# Pure functions for WL-3:
#   map_severity        — cargo-audit severity → crabcc-cron severity
#   advisory_to_finding — assemble a JSONL finding from advisory + context

# Args: cargo_audit_severity (string, possibly empty)
# Echoes: info | warn | error
map_severity() {
  case "${1,,}" in
    critical|high) echo "error" ;;
    medium)        echo "warn"  ;;
    *)             echo "info"  ;;
  esac
}

# Args: repo, advisory_json (one .vulnerabilities.list[] entry),
#       dep_chain (multi-line string), usage_count ("N" or "null")
# Echoes: one JSONL finding line.
advisory_to_finding() {
  local repo="$1" advisory="$2" dep_chain="$3" usage_count="$4"
  local audit_sev crabcc_sev advisory_id crate vuln_version fixed_version
  local title description cwe_first dep_chain_length usage_unavail_reason

  audit_sev="$(jq -r '.advisory.severity // ""' <<<"$advisory")"
  crabcc_sev="$(map_severity "$audit_sev")"
  advisory_id="$(jq -r '.advisory.id // "unknown"' <<<"$advisory")"
  crate="$(jq -r '.package.name // "unknown"' <<<"$advisory")"
  vuln_version="$(jq -r '.package.version // ""' <<<"$advisory")"
  fixed_version="$(jq -r '.versions.patched[0] // ""' <<<"$advisory")"
  description="$(jq -r '.advisory.title // ""' <<<"$advisory")"
  cwe_first="$(jq -r '.advisory.cwe[0] // ""' <<<"$advisory")"

  # Title format: "<advisory_id>: <crate> <<fixed-version> <headline>"
  # fixed_version comes from cargo audit as ">=0.15.0"; strip the ">=" so
  # the title reads "hashbrown <0.15.0 ..." rather than "hashbrown <>=0.15.0 ...".
  local fixed_short="${fixed_version#>=}"
  title="${advisory_id}: ${crate} <${fixed_short} ${description}"

  # Dep chain length: count non-empty lines (0 if "<dep-chain unavailable>")
  if [[ "$dep_chain" == "<dep-chain unavailable>" ]]; then
    dep_chain_length="null"
  else
    dep_chain_length="$(printf '%s\n' "$dep_chain" | grep -cE '^.+' || echo 0)"
  fi

  # Usage count → null vs. number
  if [[ "$usage_count" == "null" ]]; then
    usage_unavail_reason="crabcc index unavailable"
  fi

  # Build body
  local body
  body=$(cat <<EOF
${description}

$(jq -r '.advisory.description // ""' <<<"$advisory")

Vulnerable: ${crate} ${vuln_version}
Fixed in:   ${crate} ${fixed_version}

Reverse-dep chain (from cargo tree --invert):
${dep_chain}

Usage sites in this repo: ${usage_count} files (via crabcc fuzzy ${crate})

Upgrade: cargo update -p ${crate}
EOF
)

  # Build metadata as JSON object
  local meta
  if [[ "$usage_count" == "null" ]]; then
    meta="$(jq -nc \
      --arg id "$advisory_id" \
      --arg crate "$crate" \
      --arg vuln "$vuln_version" \
      --arg fixed "$fixed_version" \
      --arg audit_sev "$audit_sev" \
      --arg dcl "$dep_chain_length" \
      --arg cwe "$cwe_first" \
      --arg reason "$usage_unavail_reason" \
      '{advisory_id:$id, crate:$crate, vulnerable_version:$vuln, fixed_version:$fixed, cargo_audit_severity:$audit_sev, dep_chain_length:(if $dcl == "null" then null else ($dcl|tonumber) end), usage_site_count:null, usage_count_unavailable_reason:$reason, cwe:(if $cwe == "" then null else $cwe end)}')"
  else
    meta="$(jq -nc \
      --arg id "$advisory_id" \
      --arg crate "$crate" \
      --arg vuln "$vuln_version" \
      --arg fixed "$fixed_version" \
      --arg audit_sev "$audit_sev" \
      --arg dcl "$dep_chain_length" \
      --argjson usage "$usage_count" \
      --arg cwe "$cwe_first" \
      '{advisory_id:$id, crate:$crate, vulnerable_version:$vuln, fixed_version:$fixed, cargo_audit_severity:$audit_sev, dep_chain_length:(if $dcl == "null" then null else ($dcl|tonumber) end), usage_site_count:$usage, cwe:(if $cwe == "" then null else $cwe end)}')"
  fi

  # Final finding line
  jq -nc \
    --arg sev "$crabcc_sev" \
    --arg repo "$repo" \
    --arg title "$title" \
    --arg body "$body" \
    --argjson meta "$meta" \
    '{kind:"finding", workload:"security", repo:$repo, severity:$sev, title:$title, body:$body, metadata:$meta}'
}
```

- [ ] **Step 4: Run tests**

```bash
bats tools/crabcc-cron/tests/unit/audit_advisory.bats
shellcheck -x tools/crabcc-cron/lib/audit_advisory.sh
bats tools/crabcc-cron/tests/unit   # full sweep — no regressions
```

Expected: 11/11 new pass; shellcheck clean; existing 52 unit tests (48 WL-2 + 4 from T1) still pass → 63/63 total.

- [ ] **Step 5: Commit and push**

```bash
git add tools/crabcc-cron/lib/audit_advisory.sh \
        tools/crabcc-cron/tests/unit/audit_advisory.bats
git -c commit.gpgsign=false commit -m "feat(crabcc-cron): audit_advisory (severity + finding builder) for WL-3"
git push
```

---

## Task 4: `jobs/security.sh` — WL-3 entrypoint

**Rationale:** Wires together config-shim, repo enumeration, per-repo `git pull` / `cargo audit` / `cargo tree` / `crabcc fuzzy`, and finding emission. No unit tests — exercised by T6's e2e smoke.

**Files:**
- Create: `tools/crabcc-cron/jobs/security.sh` (executable)

- [ ] **Step 1: Write `jobs/security.sh`**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/jobs/security.sh — WL-3 entrypoint.
#
# Daily 02:00 UTC: walk every peterlodri-sec Rust repo, cargo audit,
# emit one finding per advisory + one clean-scan finding per clean repo
# + one summary finding.

set -uo pipefail

CRON_ROOT="${CRON_ROOT:-/opt/crabcc-cron}"

# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/log.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/audit_repos.sh"
# shellcheck disable=SC1091
source "${CRON_ROOT}/lib/audit_advisory.sh"

: "${SECURITY_CONFIG:=/etc/crabcc-cron/security.toml}"
: "${SECURITY_ROOT:=/srv/cron-agents/security}"
mkdir -p "$SECURITY_ROOT"

# Defensive init: ensures audit_repos never trips set -u
# even if the config shim fails to emit the SECURITY_DENY array.
# shellcheck disable=SC2034  # consumed by sourced lib/audit_repos.sh
SECURITY_DENY=()

eval "$("${CRON_ROOT}/bin/crabcc-cron-config-shim" "$SECURITY_CONFIG")"

t_start=$(date +%s)
log_info "security tick start"

repos_scanned=0
repos_clean=0
advisories_total=0

while IFS= read -r repo; do
  [[ -z "$repo" ]] && continue
  repos_scanned=$((repos_scanned + 1))
  log_info "scanning $repo"

  dir="${SECURITY_ROOT}/${repo}"
  if [[ -d "$dir/.git" ]]; then
    if ! git -C "$dir" pull --quiet 2>/dev/null; then
      log_warn "$repo: git pull failed, re-cloning"
      rm -rf "$dir"
      if ! gh repo clone "peterlodri-sec/${repo}" "$dir" -- --quiet 2>/dev/null; then
        emit_finding error security "peterlodri-sec/$repo" "clone failed" \
          "Failed to clone or pull peterlodri-sec/${repo}." '{}'
        continue
      fi
    fi
  else
    if ! gh repo clone "peterlodri-sec/${repo}" "$dir" -- --quiet 2>/dev/null; then
      emit_finding error security "peterlodri-sec/$repo" "clone failed" \
        "Failed to clone peterlodri-sec/${repo}." '{}'
      continue
    fi
  fi

  # Skip if no Cargo.lock — cargo audit needs it.
  if [[ ! -f "$dir/Cargo.lock" ]]; then
    emit_finding info security "peterlodri-sec/$repo" "skipped (no Cargo.lock)" \
      "Repo has Cargo.toml but no Cargo.lock; cargo audit needs the lockfile." '{}'
    continue
  fi

  # Refresh crabcc index (best effort; failures only impact usage counts).
  index_available=1
  if ! (cd "$dir" && crabcc index --refresh >/dev/null 2>&1); then
    log_warn "$repo: crabcc index --refresh failed; usage counts disabled"
    index_available=0
  fi

  # Run cargo audit — non-zero exit is normal when advisories present.
  audit_json="$(cd "$dir" && cargo audit --json 2>/dev/null || true)"
  if [[ -z "$audit_json" ]]; then
    emit_finding error security "peterlodri-sec/$repo" "cargo-audit failed" \
      "cargo audit produced empty output for ${repo}." '{}'
    continue
  fi
  if ! jq -e . <<<"$audit_json" >/dev/null 2>&1; then
    emit_finding error security "peterlodri-sec/$repo" "cargo-audit failed" \
      "cargo audit produced malformed JSON for ${repo}." '{}'
    continue
  fi

  # Walk advisories.
  advisories_in_repo=0
  while IFS= read -r advisory; do
    [[ -z "$advisory" ]] && continue
    advisories_in_repo=$((advisories_in_repo + 1))

    crate="$(jq -r '.package.name' <<<"$advisory")"

    # Dep chain via cargo tree --invert.
    dep_chain="$(cd "$dir" && cargo tree --invert -p "$crate" --no-default-features 2>/dev/null || echo '<dep-chain unavailable>')"
    [[ -z "$dep_chain" ]] && dep_chain="<dep-chain unavailable>"

    # Usage count via crabcc fuzzy.
    if (( index_available == 1 )); then
      usage_count="$(cd "$dir" && crabcc fuzzy "$crate" 2>/dev/null | wc -l | tr -d ' ' || echo 'null')"
      [[ -z "$usage_count" ]] && usage_count="null"
    else
      usage_count="null"
    fi

    advisory_to_finding "peterlodri-sec/$repo" "$advisory" "$dep_chain" "$usage_count"
  done < <(jq -c '.vulnerabilities.list[]?' <<<"$audit_json")

  advisories_total=$((advisories_total + advisories_in_repo))

  if (( advisories_in_repo == 0 )); then
    repos_clean=$((repos_clean + 1))
    direct_deps="$(cd "$dir" && cargo tree --depth 1 2>/dev/null | wc -l | tr -d ' ' || echo 0)"
    transitive_deps="$(cd "$dir" && cargo tree 2>/dev/null | wc -l | tr -d ' ' || echo 0)"
    emit_finding info security "peterlodri-sec/$repo" "security scan clean" \
      "cargo audit found no advisories. ${direct_deps} direct deps, ${transitive_deps} transitive." \
      "$(jq -nc --argjson dd "$direct_deps" --argjson td "$transitive_deps" \
          '{direct_deps:$dd, transitive_deps:$td}')"
  fi
done < <(enumerate_audit_repos)

duration=$(( $(date +%s) - t_start ))

emit_finding info security "" "security tick complete" \
  "Scanned $repos_scanned repos, $advisories_total advisories, $repos_clean repos clean. Duration: ${duration}s." \
  "$(jq -nc \
      --argjson scanned "$repos_scanned" \
      --argjson advisories "$advisories_total" \
      --argjson clean "$repos_clean" \
      --argjson dur "$duration" \
      '{repos_scanned:$scanned, advisories_total:$advisories, repos_clean:$clean, duration_s:$dur}')"

log_info "security tick done in ${duration}s"
exit 0
```

- [ ] **Step 2: Make executable, run lint**

```bash
chmod +x tools/crabcc-cron/jobs/security.sh
task cron-lint
```

Expected: shellcheck clean. `cron-lint` picks the new file up automatically via the glob.

- [ ] **Step 3: Sanity sweep**

```bash
task cron-test   # 63/63 from prior tasks still pass
```

Expected: 63/63 (no unit tests added by T4; e2e comes in T6).

- [ ] **Step 4: Commit and push**

```bash
git add tools/crabcc-cron/jobs/security.sh
git -c commit.gpgsign=false commit -m "feat(crabcc-cron): security.sh WL-3 dispatcher entrypoint"
git push
```

---

## Task 5: Deploy artifacts (config template + cron entry + installer preflight)

**Files:**
- Create: `tools/crabcc-cron/deploy/security.toml.example`
- Modify: `tools/crabcc-cron/deploy/crabcc-cron.cron` (add daily 02:00 UTC line)
- Modify: `tools/crabcc-cron/deploy/install.sh` (add `cargo-audit` preflight check)
- Modify: `tools/crabcc-cron/README.md` (one-line mention of the security workload)

- [ ] **Step 1: Write `deploy/security.toml.example`**

```toml
# /etc/crabcc-cron/security.toml — WL-3 dispatcher config.

[security_deny]
# Repos to skip. Match against the bare repo name (no owner prefix).
# Useful for sandbox/template/archive repos that shouldn't be audited.
exclude = []
```

- [ ] **Step 2: Modify `deploy/crabcc-cron.cron`**

Append after the existing WL-2 line:

```cron

# WL-3 security audit — daily 02:00 UTC.
0 2 * * * deploy  /opt/crabcc-cron/jobs/security.sh 2>&1 | tee >(systemd-cat -t crabcc-cron-security) | /opt/crabcc-cron/bin/crabcc-cron-emit
```

- [ ] **Step 3: Modify `deploy/install.sh`**

In the preflight block (step 6 from the A4-fix commit), add a check after the `gh` check:

```bash
if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found. Install Rust via rustup before continuing." >&2
  exit 1
fi

if ! cargo install --list 2>/dev/null | grep -q '^cargo-audit '; then
  echo "WARNING: cargo-audit not installed. WL-3 (security) will fail every tick." >&2
  echo "Install with: cargo install cargo-audit" >&2
fi
```

- [ ] **Step 4: Modify `tools/crabcc-cron/README.md`**

Update the "Workloads" section (or add it if it doesn't exist) with a one-line description of the security workload:

```markdown
## Workloads

- **WL-2 OSS-fix** (`jobs/oss-fix.sh`, every 4h) — picks one eligible upstream "good first issue" and attempts a fix via opencode.
- **WL-3 security** (`jobs/security.sh`, daily 02:00 UTC) — runs `cargo audit` across every `peterlodri-sec` Rust repo and emits per-advisory findings with reverse-dep chain and crabcc usage counts.
```

If the file already has a Workloads section, add the WL-3 line. Otherwise insert this section after the "## Layout" section.

- [ ] **Step 5: Lint and commit**

```bash
shellcheck -x tools/crabcc-cron/deploy/install.sh
task cron-lint
git add tools/crabcc-cron/deploy/security.toml.example \
        tools/crabcc-cron/deploy/crabcc-cron.cron \
        tools/crabcc-cron/deploy/install.sh \
        tools/crabcc-cron/README.md
git -c commit.gpgsign=false commit -m "feat(crabcc-cron): WL-3 deploy artifacts (toml, cron, installer preflight)"
git push
```

---

## Task 6: e2e smoke test

**Files:**
- Create: `tools/crabcc-cron/tests/e2e/security_smoke.sh` (executable)

End-to-end smoke with mocked `gh`, `cargo`, `crabcc`. Two synthetic repos: one with an advisory, one clean. Asserts 3 findings emitted (1 advisory + 1 clean-scan + 1 summary).

- [ ] **Step 1: Write the smoke**

```bash
#!/usr/bin/env bash
# tools/crabcc-cron/tests/e2e/security_smoke.sh
#
# End-to-end smoke for WL-3 security workload.
# Mocks: gh, cargo, crabcc.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd -P)"
TMPD="$(mktemp -d)"
trap 'rm -rf "$TMPD"' EXIT

mkdir -p "$TMPD/bin"

# Fake gh: repo list returns 2 repos; api Cargo.toml exists for both;
# repo clone creates a minimal cargo project under the target dir.
cat >"$TMPD/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "repo list")
    cat <<'JSON'
[{"name":"repo-with-advisory","defaultBranch":"main"},{"name":"repo-clean","defaultBranch":"main"}]
JSON
    ;;
  "api "*"/contents/Cargo.toml"*)
    echo '{"name":"Cargo.toml"}'
    ;;
  "repo clone")
    # The job calls: gh repo clone <owner/name> <dir> -- --quiet
    # → $3="<owner/name>", $4="<dir>", $5="--", $6="--quiet".
    dest="$4"
    mkdir -p "$dest"
    cd "$dest"
    git init --quiet
    git config user.email "bot@example.com"
    git config user.name  "bot"
    cat >Cargo.toml <<'C'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
C
    cat >Cargo.lock <<'C'
# Minimal lock; real cargo audit would reject this, but our fake handles it.
version = 3
C
    mkdir -p src; echo 'fn main(){}' >src/main.rs
    git add -A; git commit --quiet -m "init"
    ;;
esac
EOF
chmod +x "$TMPD/bin/gh"

# Fake cargo: audit returns 1 advisory for repo-with-advisory, empty for repo-clean.
# tree returns a 2-line chain. The fake routes by CWD basename.
cat >"$TMPD/bin/cargo" <<'EOF'
#!/usr/bin/env bash
repo_basename="$(basename "$PWD")"
case "$1" in
  audit)
    if [[ "$repo_basename" == "repo-with-advisory" ]]; then
      cat <<'JSON'
{"vulnerabilities":{"list":[
  {"advisory":{"id":"RUSTSEC-2024-0388","title":"use-after-free in hashbrown","description":"UAF","severity":"high","cwe":["CWE-416"]},
   "package":{"name":"hashbrown","version":"0.14.5"},
   "versions":{"patched":[">=0.15.0"]}}
]}}
JSON
    else
      cat <<'JSON'
{"vulnerabilities":{"list":[]}}
JSON
    fi
    exit 0
    ;;
  tree)
    echo "fixture v0.1.0"
    echo "└── hashbrown v0.14.5"
    exit 0
    ;;
esac
EOF
chmod +x "$TMPD/bin/cargo"

# Fake crabcc: index --refresh exits 0, fuzzy returns a few lines.
cat >"$TMPD/bin/crabcc" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  index) exit 0 ;;
  fuzzy)
    # Pretend crate $2 is referenced in 3 files.
    echo "src/main.rs:1: fn main(){}"
    echo "src/foo.rs:2: use foo;"
    echo "src/bar.rs:3: use bar;"
    exit 0
    ;;
esac
EOF
chmod +x "$TMPD/bin/crabcc"

# Build the config.
mkdir -p "$TMPD/etc"
cat >"$TMPD/etc/security.toml" <<'TOML'
[security_deny]
exclude = []
TOML

# Invoke.
PATH="$TMPD/bin:$PATH" \
CRON_ROOT="$ROOT" \
SECURITY_CONFIG="$TMPD/etc/security.toml" \
SECURITY_ROOT="$TMPD/repos" \
bash "$ROOT/jobs/security.sh" > "$TMPD/out.jsonl" 2> "$TMPD/err.log"

echo "--- stdout ---"
cat "$TMPD/out.jsonl"
echo "--- stderr ---"
cat "$TMPD/err.log"

# Assert: exactly 3 findings (1 advisory + 1 clean + 1 summary).
findings=$(grep -c '"kind":"finding"' "$TMPD/out.jsonl" || echo 0)
[[ "$findings" -eq 3 ]] || { echo "FAIL: expected 3 findings, got $findings"; exit 1; }

# Assert: one finding is the RUSTSEC advisory.
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e 'select(.metadata.advisory_id == "RUSTSEC-2024-0388") | .severity == "error"' >/dev/null \
  || { echo "FAIL: expected RUSTSEC-2024-0388 finding with severity=error"; exit 1; }

# Assert: one finding is the clean-scan.
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e 'select(.title == "security scan clean") | .severity == "info"' >/dev/null \
  || { echo "FAIL: expected security scan clean finding"; exit 1; }

# Assert: one finding is the summary.
grep '"kind":"finding"' "$TMPD/out.jsonl" | jq -e 'select(.title == "security tick complete") | .metadata.repos_scanned == 2' >/dev/null \
  || { echo "FAIL: expected security tick complete finding with repos_scanned=2"; exit 1; }

echo "PASS: security workload end-to-end smoke"
```

- [ ] **Step 2: Make executable and run**

```bash
chmod +x tools/crabcc-cron/tests/e2e/security_smoke.sh
bash tools/crabcc-cron/tests/e2e/security_smoke.sh
```

Expected: prints `PASS: security workload end-to-end smoke`.

- [ ] **Step 3: Full sweep**

```bash
task cron-test                                          # 63/63 unit tests
bash tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh      # WL-2 smoke still passes
bash tools/crabcc-cron/tests/e2e/security_smoke.sh     # WL-3 smoke passes
task cron-lint                                          # shellcheck clean
```

Expected: all green.

- [ ] **Step 4: Commit and push**

```bash
git add tools/crabcc-cron/tests/e2e/security_smoke.sh
git -c commit.gpgsign=false commit -m "feat(crabcc-cron): WL-3 end-to-end smoke test"
git push
```

---

## Final verification

After all 6 tasks land:

- [ ] **Unit tests:** `task cron-test` → 63/63 pass.
- [ ] **WL-2 e2e:** `bash tools/crabcc-cron/tests/e2e/oss_fix_dryrun.sh` → `PASS`.
- [ ] **WL-3 e2e:** `bash tools/crabcc-cron/tests/e2e/security_smoke.sh` → `PASS`.
- [ ] **Lint:** `task cron-lint` clean.

- [ ] **Spec coverage check**:

  | Spec section | Task |
  |---|---|
  | 3.1 Audit target (gh repo list + Cargo.toml filter + deny) | T1 (deny config), T2 (enumeration) |
  | 3.2 Deployment target (Hetzner, reuses shared layer) | T5 (cron line, installer preflight) |
  | 3.3 Cadence (daily 02:00 UTC) | T5 (crontab) |
  | 4.1 New files | All 6 tasks |
  | 4.2 Daily flow | T4 (jobs/security.sh) |
  | 4.3 Index policy (build on Hetzner, refresh nightly) | T4 (crabcc index --refresh call) |
  | 5.1 Per-advisory finding shape | T3 (advisory_to_finding) |
  | 5.2 Severity mapping | T3 (map_severity) |
  | 5.3 Clean-scan finding | T4 |
  | 5.4 Tick-summary finding | T4 |
  | 5.5 Idempotency (sha256 of title) | T3 (title includes advisory_id) + T4 (relies on emit) |
  | 6 Failure handling (per-repo isolation, no state) | T4 |
  | 7 Cron entry | T5 |
  | 8 Out of scope | Documented in spec — no tasks needed |
  | 9 Testing | T1-T3 bats + T6 e2e |
  | 10 Implementation order | This plan's task ordering |
  | 11 Open implementation questions | Addressed during T4 (cargo-audit preflight in T5) |

- [ ] **Open PR** from `claude/spec-crabcc-cron-wl3-2026-05-17` into `main` (or whatever branch you're working on).
