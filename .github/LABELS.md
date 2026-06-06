# Issue & PR labels

> The canonical pattern is set by issues #236–#242. Every new issue must satisfy
> the rule: **type + (priority) + (milestone)**. Bots and templates pre-fill the
> common cases; everything below is manual triage guidance.

## Rule of three

Every issue should carry, at minimum:

1. **One type label** (what kind of work)
2. **One priority label** when actionable (`priority:high|medium|low`)
3. **One milestone label** if scheduled (`v2.5`, `v3.0`, `v3.1`)

Optional, additive labels: `lang:rust|go|python`, `epic`, `security`, `good first issue`, `help wanted`, `dependencies`, `ci`, `docs`.

## Type labels

| Label | Use for | Title prefix |
|---|---|---|
| `bug` | Something is broken or behaves incorrectly | `fix(scope):` |
| `enhancement` | Umbrella for non-bug work — pair with one of the rows below | — |
| `feature` | Net-new capability | `feat(scope):` |
| `performance` | Speed / memory / disk | `perf(scope):` |
| `refactor` | Code shape change, no behavior change | `refactor(scope):` |
| `test` | Test infra or new tests | `test(scope):` |
| `ci` | CI workflow / GitHub Actions | `ci:` |
| `dependencies` | Dependency-version maintenance | `deps:` |
| `docs` / `documentation` | Documentation work | `docs(scope):` |
| `chore` | Anything that doesn't fit elsewhere | `chore(scope):` |
| `rfc` | Design proposal needing discussion | `rfc(scope):` |
| `epic` | Cross-cutting umbrella tracking child issues | `epic(scope):` |
| `security` | Security-relevant fix or upgrade | (any) |

The pairing **`enhancement` + (one of: `feature` / `performance` / `ci` / `dependencies` / `docs`)** is the dominant pattern in #236–#242. Keep it.

## Scope vocabulary (used in titles, not as labels)

`cli`, `viz`, `agents`, `memory`, `mcp`, `telegram`, `mobile`, `extension`, `cua`, `tts`, `events`, `netlog`, `security`, `service-discovery`, `mdns`, `jobs`, `docs`.

Multiple scopes are comma-joined: `feat(mcp,telegram): …` (see #204).

## Priority

| Label | Meaning |
|---|---|
| `priority:high` | Address before next release |
| `priority:medium` | Address within a release cycle |
| `priority:low` | Nice-to-have, no deadline |

## Milestones

`v2.5`, `v3.0`, `v3.1`. Used both as GitHub milestones (for the project board) and labels (for fast filtering on the issues list). Keep them in sync.

## Examples (#236–#242)

| # | Title | Labels |
|---|---|---|
| 236 | `perf(viz): senior-Rust review of …graph.rs` | `enhancement, lang:rust, performance, v3.0, priority:medium` |
| 238 | `feat(mobile): Happy-inspired agent session forwarding` | `enhancement, feature, v3.0` |
| 242 | `feat(tts): TTS word-timing alignment` | `enhancement, feature, v3.0, priority:low` |

## Triage workflow

New issues land via the templates (`.github/ISSUE_TEMPLATE/`), which pre-fill the type. Triage:

1. Confirm the type is right (or add the missing one).
2. Assign priority unless it's a parking-lot idea.
3. If scheduled, attach the milestone *and* the matching label.
4. Add `lang:*` only when the change is language-scoped.
5. Add `epic` only if there are 3+ child issues planned.

## Re-labeling old issues

Use `scripts/sync-issue-labels.sh` (see file header for the mapping) to backfill consistent labels. Re-runs are idempotent.
