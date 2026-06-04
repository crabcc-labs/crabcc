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
  # Include primaryLanguage to filter Rust repos in jq instead of making a
  # per-repo `gh api .../Cargo.toml` existence check (N+1 calls → 1 call).
  repos="$(gh repo list peterlodri-sec \
    --no-archived \
    --limit 200 \
    --json name,defaultBranch,primaryLanguage 2>/dev/null \
    || echo '[]')"

  while IFS= read -r item; do
    name="$(jq -r '.name' <<<"$item")"
    primary="$(jq -r '.primaryLanguage.name // empty' <<<"$item")"
    [[ -z "$name" ]] && continue

    # Denylist check.
    denied=0
    for ex in "${SECURITY_DENY[@]:-}"; do
      [[ "$ex" == "$name" ]] && { denied=1; break; }
    done
    (( denied == 1 )) && continue

    if [[ "$primary" == "Rust" ]]; then
      # Fast path: GitHub detected Rust as primary — no extra API call needed.
      printf '%s\n' "$name"
    elif [[ -z "$primary" ]]; then
      # Unknown primary language (new/tiny/polyglot repo) — fall back to the
      # authoritative Cargo.toml existence check to avoid false negatives.
      gh api "repos/peterlodri-sec/$name/contents/Cargo.toml" &>/dev/null \
        && printf '%s\n' "$name"
    fi
    # Any other known primary language → not a Rust project, skip.
  done < <(jq -c '.[]' <<<"$repos")
  return 0
}
