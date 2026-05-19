# Source from ~/.zshrc on your Mac:
#   source ~/path/to/crabcc/install/mac/crabcc-env.zsh
#
# Keeps indexes under ~/.crabcc (never in worktrees or .crabcc/ checkouts).
# All git worktrees of the same repo share one index (keyed by origin URL).

export CRABCC_HOME="${CRABCC_HOME:-$HOME/.crabcc}"
export CRABCC_LAYOUT=centralised

# Optional: only build the index when you run `crabcc index` explicitly.
# Uncomment if agents should not auto-index on first sym/refs call:
# export CRABCC_NO_AUTO_INDEX=1
