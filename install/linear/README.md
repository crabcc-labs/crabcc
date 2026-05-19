# Linear project + GitHub issue sync (crabcc)

## Linear project

**Project:** [crabcc](https://linear.app/vibepeter/project/crabcc-23377014338b)  
**Team:** Vibe.peter (`VIB`)  
**Repo:** https://github.com/peterlodri-sec/crabcc

Backfilled issues use the title prefix `GH-<number>:` and a GitHub link attachment.

## GitHub ↔ Linear sync (enabled)

Bidirectional sync is on for `peterlodri-sec/crabcc` → **Vibe.peter** / project **crabcc**.

- New GitHub issues appear in Linear automatically.
- Closing or reopening in either system updates the other.
- PR links work when GitHub integration includes pull requests.

Docs: https://linear.app/docs/github-integration

## Bulk backfill (script)

Use only for **initial import** or re-label passes — not for day-to-day tracking (sync handles that).

```bash
export LINEAR_API_KEY=lin_api_...   # Linear → Settings → API → Personal API keys
task linear-sync-dry-run            # preview
task linear-sync                    # import missing GH-<n>: issues
```

Or directly:

```bash
python3 tools/linear/sync_github_issues.py --dry-run
python3 tools/linear/sync_github_issues.py
```

Idempotent: skips Linear issues whose title already starts with `GH-<n>:`.

## Labels

| Linear label | Meaning |
|--------------|---------|
| `github-sync` | Mirrored / backfilled from GitHub |
| `Bug` / `Feature` | Mapped from GitHub `bug` / `enhancement` / `feature` |
| `epic` | GitHub `epic` label |

## PR workflow

Link PRs in Linear via `Fixes #123` / `Closes VIB-42` in PR bodies, or branch names containing `vib-` / `gh-` — Linear picks up linked PRs when GitHub integration includes pull requests.
