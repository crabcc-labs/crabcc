# Linear project + GitHub issue sync (crabcc)

## Linear project

**Project:** [crabcc](https://linear.app/vibepeter/project/crabcc-23377014338b)  
**Team:** Vibe.peter (`VIB`)  
**Repo:** https://github.com/peterlodri-sec/crabcc

Synced issues use the title prefix `GH-<number>:` and link back to GitHub.

## Direction: GitHub → Linear only

**GitHub is the source of truth.** Status, title, and description flow from GitHub into Linear. Changes made only in Linear are **not** written back to GitHub.

To avoid conflicts:

1. In **Linear → Settings → Integrations → GitHub**, do **not** enable bidirectional issue sync for `crabcc` (or disable it if already on).
2. Rely on this repo’s workflow (`.github/workflows/linear-sync.yml`) and script instead.

## CI / automation (repo secrets)

The workflow uses repository secrets (already configured):

| Secret | Used for |
|--------|----------|
| `LINEAR_API_KEY` | Linear GraphQL API |
| `GH_PERSONAL_TOKEN` | GitHub REST API (`issues` read) |

Triggers: `issues` events (opened, edited, closed, reopened, labeled), `workflow_dispatch`, and a 6-hour catch-up schedule.

## Local / manual sync

```bash
export LINEAR_API_KEY=lin_api_...
export GH_PERSONAL_TOKEN=ghp_...   # or use `gh auth login`
task linear-sync-dry-run
task linear-sync
```

Single issue (same as CI on an event):

```bash
python3 tools/linear/sync_github_issues.py --issue-number 551
```

Idempotent: creates missing `GH-<n>:` issues; updates title/description/state when GitHub changes.

## Labels

| Linear label | Meaning |
|--------------|---------|
| `github-sync` | Mirrored from GitHub |
| `Bug` / `Feature` | Mapped from GitHub `bug` / `enhancement` / `feature` |
| `epic` | GitHub `epic` label |

## PR workflow

Reference GitHub issues in PRs with `Fixes #123`. Linear issues stay linked via the `GH-<n>:` title and GitHub URL in the description; no bidirectional PR sync required.
