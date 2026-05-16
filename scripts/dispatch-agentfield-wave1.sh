#!/usr/bin/env bash
# Dispatch the Wave-1 tasks from docs/REFACTOR-treesitter-3.2.0.md to the
# swe-planner agent on AgentField. Three tasks fire in parallel (T1, T4, T6)
# — they touch disjoint files and have no dependency between them.
#
# Usage:
#   AF_URL=http://100.67.51.123:8080 \
#   AGENTFIELD_API_KEY=<your-key> \
#   scripts/dispatch-agentfield-wave1.sh
#
# The script prints the execution_id for each dispatch and exits 0 on
# successful queueing. Polling for completion is left to the AgentField UI
# (or `gh pr list` once the agent opens its draft PR).
#
# Following the same dispatch pattern as
# AgentField's own examples/e2e_resilience_tests/run_tests.sh — `curl -X POST`
# against /api/v1/execute/async/<agent>.<reasoner> with a JSON body.

set -euo pipefail

# ── Config ─────────────────────────────────────────────────────────────────
AF_URL="${AF_URL:-http://100.67.51.123:8080}"
AF_NODE="${AF_NODE:-swe-planner}"
AF_REASONER="${AF_REASONER:-build}"
REPO_URL="${REPO_URL:-https://github.com/peterlodri-sec/crabcc.git}"
BASE_BRANCH="${BASE_BRANCH:-fix/cli-lookup-refs-followups}"
PLAN_URL="https://github.com/peterlodri-sec/crabcc/blob/${BASE_BRANCH}/docs/REFACTOR-treesitter-3.2.0.md"

if [[ -z "${AGENTFIELD_API_KEY:-}" ]]; then
    echo "error: AGENTFIELD_API_KEY is not set" >&2
    echo "  export it from your secrets store, e.g.:" >&2
    echo "    export AGENTFIELD_API_KEY=\$(pass agentfield/api-key)" >&2
    exit 2
fi

# ── Dispatch helper ────────────────────────────────────────────────────────
# AgentField REST contract:
#   POST /api/v1/execute/async/<node_id>.<reasoner_id>
#   Headers: X-API-Key, Content-Type: application/json
#   Body:    {"input": {...}}
# The async endpoint returns 202 + {"execution_id": "..."} which the
# AgentField UI can use to follow the run.
dispatch_task() {
    local task_id="$1"
    local goal="$2"
    local additional_context="$3"

    local body
    body=$(python3 -c '
import json, sys
print(json.dumps({
    "input": {
        "goal": sys.argv[1],
        "repo_url": sys.argv[2],
        "additional_context": sys.argv[3],
        "config": {"github_pr_base": sys.argv[4]},
    }
}))' "$goal" "$REPO_URL" "$additional_context" "$BASE_BRANCH")

    local target="${AF_NODE}.${AF_REASONER}"
    local response
    response=$(curl -sS --max-time 15 \
        -X POST "${AF_URL}/api/v1/execute/async/${target}" \
        -H "X-API-Key: ${AGENTFIELD_API_KEY}" \
        -H "Content-Type: application/json" \
        -d "$body")

    local execution_id
    execution_id=$(echo "$response" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("execution_id","?"))' 2>/dev/null || echo "?")

    printf '%-4s  execution_id=%s\n' "$task_id" "$execution_id"

    if [[ "$execution_id" == "?" ]]; then
        echo "    raw response: $response" >&2
    fi
}

# ── Task briefs ────────────────────────────────────────────────────────────
# Each `additional_context` is a self-contained prompt — the same content the
# user would paste into the AgentField web UI. The full task spec lives in
# the plan file at $PLAN_URL; we reference it but keep the brief inline so
# the agent doesn't need to fetch external docs to start.

T1_GOAL="Execute task T1 from ${PLAN_URL} — re-apply the docs fix dropped by the PR #547 squash merge."
read -r -d '' T1_CTX <<EOF || true
Branch from: ${BASE_BRANCH}
Output: draft PR against main, title:
  docs(3.2.0): correct ref_edges_built mechanism in CHANGELOG + HOW_IT_WORKS

The PR #547 squash kept the pre-correction text. The shipped implementation
uses meta('ref_edges_built'), written only by Store::mark_ref_edges_built()
after full_index completes — NOT a schema_version bump.

Fix two files:

1. CHANGELOG.md (around line 22): rewrite the bullet that currently says
   "automatically detected on open via schema_version (bumped from 2 to 3)"
   to describe the ref_edges_built marker mechanism.

2. crates/crabcc-core/docs/HOW_IT_WORKS.md:
   - Add ref_edges_built to the meta-table row description.
   - Under "Schema discipline", add a bullet explaining why data-readiness
     gates are SEPARATE keys from schema_version (MCP and LSP open Store
     too — bumping the version stamp on stale rows would hide staleness).
   - In "Evolve the schema", add a follow-on rule: if a migration requires
     row contents to be rebuilt (not just a column shape change), add a
     dedicated meta key written only post-rebuild and read into
     Store::needs_reindex.

Verify with:
  grep -n "ref_edges_built" CHANGELOG.md crates/crabcc-core/docs/HOW_IT_WORKS.md
  task fmt-check && task lint
EOF

T4_GOAL="Execute task T4 from ${PLAN_URL} — make MCP and LSP act on Store::needs_reindex."
read -r -d '' T4_CTX <<EOF || true
Branch from: ${BASE_BRANCH}
Output: draft PR against main, title:
  fix(mcp,lsp): act on Store::needs_reindex instead of silently serving stale

Today the CLI auto-wipe path lives in crates/crabcc-cli/src/main.rs:1117
but crates/crabcc-mcp/src/dispatch.rs:143 and
crates/ucracc-lsp/src/server.rs:65 both call Store::open and proceed.
On a stale index they serve empty lookup refs results with no signal.

Pick Option A from the plan (consumers run wipe-and-rebuild):
  1. Extract the CLI's wipe-and-rebuild block (main.rs:1117-1131) into
     crate::store::wipe_and_rebuild(db, root, fts_dir, compress) -> Result<Store>.
  2. Call it from MCP dispatch.rs:143 when needs_reindex == true (synchronous
     is fine; MCP is per-request).
  3. Call it from LSP server.rs:65 on a background thread; refuse
     textDocument/references and callHierarchy/incomingCalls with a
     typed ResponseError until rebuild completes. window/logMessage progress.

If you find a blocker for Option A, switch to Option B (typed error,
no rebuild) and document the reason in the PR description.

Required tests:
  - crates/crabcc-mcp/tests/<new>.rs: stale DB (delete ref_edges_built row),
    call lookup.refs, assert rebuild-then-result.
  - crates/ucracc-lsp/tests/integration.rs: same scenario via the LSP harness.

Verify:
  cargo test -p crabcc-mcp --tests
  cargo test -p ucracc-lsp --tests
  task smoke
EOF

T6_GOAL="Execute task T6 from ${PLAN_URL} — re-check whether gpui-component pins tree-sitter = 0.26."
read -r -d '' T6_CTX <<EOF || true
Branch from: main (independent of the followups branch)
Output: EITHER a draft PR OR a tracking issue, depending on findings.

Step 1: inspect crates/crabcc-desktop/Cargo.toml for the current
gpui-component source (likely a git rev). Check that repo's current
tree-sitter pin (upstream: github.com/longbridge/gpui-component, but
verify the URL via the Cargo.toml).

Step 2a — if upstream is on tree-sitter = "0.26" or later:
  - Bump crates/crabcc-desktop/Cargo.toml's gpui-component rev.
  - Re-add the crate to workspace 'members' in the root Cargo.toml.
  - Update the explanatory comment at Cargo.toml:12-28 to match the new state.
  - Run: cargo check --workspace && cd crates/crabcc-desktop && cargo check
  - Open a draft PR titled:
      chore(desktop): rejoin workspace — gpui-component on tree-sitter 0.26

Step 2b — if upstream is still on 0.25:
  - Do NOT bump.
  - File a tracking issue on this repo titled:
      "tree-sitter 0.26 unification blocked on gpui-component"
  - Link the upstream's tree-sitter dependency line.
  - Update the comment in Cargo.toml with the current upstream pin and a
    follow-up cadence (e.g. "next check 2026-08").
  - No PR.

Do NOT force unification by pinning down to 0.25 — workspace grammar crates
(tree-sitter-bash, tree-sitter-java, tree-sitter-swift) only ship API-
compatible builds against >=0.26.
EOF

# ── Fire (sequential dispatch, agent runs in parallel) ──────────────────────
echo "Dispatching Wave 1 to ${AF_URL} (${AF_NODE}.${AF_REASONER})..."
echo "Base branch: ${BASE_BRANCH}"
echo "Plan: ${PLAN_URL}"
echo

dispatch_task "T1" "$T1_GOAL" "$T1_CTX"
dispatch_task "T4" "$T4_GOAL" "$T4_CTX"
dispatch_task "T6" "$T6_GOAL" "$T6_CTX"

echo
echo "All three queued. Track at ${AF_URL}/ui/agents or via:"
echo "  curl -H \"X-API-Key: \$AGENTFIELD_API_KEY\" ${AF_URL}/api/v1/executions/<execution_id>"
