# macOS dev — centralised local index

Use this when you work across **git worktrees** (Cursor, orchestrator, main
clone) and do not want a `.crabcc/` directory (or a full re-index) in every
checkout.

## One-time shell setup

```bash
# Add to ~/.zshrc
source /path/to/crabcc/install/mac/crabcc-env.zsh
```

This sets:

| Variable | Effect |
|----------|--------|
| `CRABCC_HOME` | `~/.crabcc` — all index artifacts |
| `CRABCC_LAYOUT=centralised` | Never use `<repo>/.crabcc/` even if it exists |

Indexes live at:

```text
~/.crabcc/repos/<repo-slug>-<hash6>/index.db
```

`<hash6>` comes from `remote.origin.url`, so **every worktree of `peterlodri-sec/crabcc`
shares one index**.

## Build the index once

From any worktree (main or Cursor):

```bash
cd /path/to/any/crabcc/checkout
crabcc index
# or: crabcc index --root .
```

## Clean old in-repo indexes (optional)

```bash
# Main repo + worktrees — safe once centralised layout is active
find ~/workspace ~/.cursor/worktrees -path '*/crabcc*/.crabcc' -type d 2>/dev/null
# inspect, then:
# find ... -exec rm -rf {} +
```

`.crabcc/` remains in `.gitignore`; it simply will not be created when
`CRABCC_LAYOUT=centralised` is set.

## Verify

```bash
crabcc lookup outline src/lib.rs 2>&1 | head -1
# Should mention [key=crabcc-XXXXXX] not [in-repo]
```
