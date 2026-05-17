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
