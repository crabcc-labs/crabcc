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

The commands below land incrementally — the lint/test targets exit cleanly today
but only do real work once tasks A2–B7 in
`docs/superpowers/plans/2026-05-17-crabcc-cron-shared-and-oss-fix.md` are merged.

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
