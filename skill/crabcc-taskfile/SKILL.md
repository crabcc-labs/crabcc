---
name: crabcc-taskfile
description: Use the project's Taskfile.yml (https://taskfile.dev) for build / test / lint / bench / release / install / docs operations on this repo. Triggers when the user asks to "build", "run tests", "lint", "format", "smoke test", "bench", "profile", "cut a release", "install crabcc", "install aliases", "regenerate docs", "open the viz dashboard", or any request that maps onto a `task <name>` entry. Skip for ad-hoc `cargo` invocations the user explicitly requested as `cargo …` — Taskfile is the canonical entry point but not the only one.
---

# crabcc-taskfile — every daily-driver action lives behind `task <name>`

`Taskfile.yml` (root of this repo) is the **canonical action surface**.
Always prefer `task <name>` over a hand-rolled `cargo`/`bash` invocation:
the Taskfile entries pin profiles (LTO, dev-fast), mirror CI gates,
capture output to `.summary/`, and chain dependencies safely.

Install once: `brew install go-task` or
`go install github.com/go-task/task/v3/cmd/task@latest`.

> Companion skills:
> - [`skill/crabcc/SKILL.md`](../crabcc/SKILL.md) — symbol-aware code lookups.
> - [`skill/warp-speed-audit/SKILL.md`](../warp-speed-audit/SKILL.md) — Rust perf audit.
> - [`skill/rust-logging-audit/SKILL.md`](../rust-logging-audit/SKILL.md) — logging / tracing audit.
> All three skills assume the build / test commands below.

---

## Tool ladder — the question → task map

| Question / intent                                | Task                          |
|--------------------------------------------------|-------------------------------|
| "build it"                                       | `task` *(= build + test)*     |
| "release build only"                             | `task build` *(LTO=fat)*      |
| "fast iteration build"                           | `task build-fast` *(`-O1`)*   |
| "run all tests"                                  | `task test`                   |
| "format / format check"                          | `task fmt` / `task fmt-check` |
| "lint" / "clippy"                                | `task lint` *(`-D warnings`)* |
| "local CI" / "before pushing"                    | `task ci` or `task local-ci`  |
| "quick local CI"                                 | `task local-ci-quick`         |
| "before opening a PR"                            | `task prep-pr`                |
| "install crabcc to ~/.cargo/bin"                 | `task install` *(auto-installs shell aliases too)* |
| "install shell aliases (grep→rg, find→fd, …)"    | `task aliases`                |
| "are aliases installed?"                         | `task aliases-check`          |
| "smoke test"                                     | `task smoke`                  |
| "memory CLI smoke"                               | `task memory-smoke`           |
| "memory bench (R@5 gate)"                        | `task memory-bench`           |
| "FSST gate bench"                                | `task bench-compress`         |
| "raw-CLI A/B bench (vs grep/rg/fd/find)"         | `task bench` / `task bench-all` |
| "full bench sweep"                               | `task bench-e2e`              |
| "live call-graph dashboard at /live"             | `task viz` *(opens browser)*  |
| "simulate agent traffic against /live"           | `task viz-sim`                |
| "profile `crabcc index` with samply"             | `task profile-index`          |
| "profile `crabcc memory search`"                 | `task profile-memory-search`  |
| "flamegraph (sudo / DTrace path)"                | `task flamegraph-index`       |
| "open rustdoc in browser"                        | `task doc`                    |
| "regenerate long-form docs (README/AGENTS/…)"    | `task docs-refresh`           |
| "diagnose dev install"                           | `task doctor`                 |
| "audit dev tools (jq/yq/rg/gh/…)"                | `task check-deps`             |
| "code coverage (HTML)"                           | `task coverage`               |
| "code coverage (text summary)"                   | `task coverage FORMAT=text`   |
| "fuzz target compile-check"                      | `task fuzz-check`             |
| "FSST: train + re-encode + report"               | `task compress`               |
| "MCP OpenAPI spec"                               | `task openapi`                |
| "current workspace version"                      | `task version`                |
| "cut a release tag"                              | `task release VERSION=x.y.z`  |
| "verify the release gate locally"                | `task verify-gate`            |
| "install.sh upgrade idempotency check"           | `task install-upgrade-smoke`  |
| "pack repo to .repomix/crabcc.xml"               | `task repomix`                |
| "manpage preview / install"                      | `task man` / `task install-man` |
| "uninstall everything"                           | `task uninstall`              |
| "clean target/ + bench results"                  | `task clean`                  |

For everything not in the table, run `task --list` once and re-grep.

---

## Variable overrides (the ones you'll actually use)

| Variable          | Default                          | Used by                              |
|-------------------|----------------------------------|--------------------------------------|
| `REPO_FIXTURE`    | `~/workspace/mc-mothership`      | `bench`, `bench-compress`, `profile-*` |
| `VERSION`         | *(required)*                     | `release`                            |
| `MODE`            | task-specific (`check`, `install`, `print`, `remove`, `strict`, `json`) | `aliases`, `doctor`, `check-deps` |
| `DATASET`         | bundled synthetic                | `memory-bench`                       |
| `THRESHOLD`       | `0.966`                          | `memory-bench`                       |
| `FORMAT`          | `html`                           | `coverage`                           |
| `PORT`            | `7878`                           | `viz`                                |
| `NO_OPEN`         | unset                            | `viz`                                |
| `OUT`             | `.repomix/crabcc.xml`            | `repomix`                            |
| `MODEL`           | `sonnet`                         | `docs-refresh`                       |
| `N`, `Q`          | `200`, `"fox jumps over"`        | `profile-memory-search`              |
| `NO_DOC`          | `0`                              | `prep-pr`                            |

Pattern: `task <name> VAR=value` (e.g. `task viz PORT=8080`,
`task memory-bench DATASET=path/to/oracle.json`,
`task release VERSION=2.7.0`).

---

## Decision rules

1. **Use a task when one exists.** Don't paraphrase a task with raw
   `cargo` — the Taskfile entry is the maintained version.
2. **`task ci` mirrors GH Actions** (fmt + clippy + test + smoke +
   memory-smoke). Run it before suggesting a push.
3. **`task prep-pr` is the pre-PR gate** — adds doc-build + captures
   output to `.summary/prep-pr.txt` for paste-into-PR-body use.
4. **`task install` is the bootstrap** — builds, installs the binary,
   AND idempotently installs shell aliases. Re-running is safe.
5. **`task local-ci` snapshots to `.summary/local-ci.txt`** for PR
   descriptions when GH CI is unavailable (rate-limited / over budget).
6. **Profiles**: `task profile-index` (samply, no sudo on macOS) is the
   default; `task flamegraph-index` only when DTrace/perf is needed.
7. **Bench gates**: `task bench-compress` runs the FSST off-vs-on gate
   in single-trial mode; `task verify-gate` is the full release-gate
   re-run. Both expect `REPO_FIXTURE` to be a real Rust/multi-lang repo.

---

## When NOT to use Taskfile

- **One-off `cargo` flags** the Taskfile doesn't cover (e.g.
  `cargo expand`, `cargo asm`). Run those raw.
- **Inside a sub-agent prompt** that doesn't have `task` on PATH —
  call the equivalent `cargo`/`bash` directly. The skill assumes a
  developer shell, not a sandboxed agent runner.
- **CI workflows** (`.github/workflows/*.yml`) — they run cargo
  directly so they're independent of the Taskfile dep on `go-task`.
  Keep them in sync manually when a Taskfile entry changes.

---

## Cross-references

- `Taskfile.yml` — source of truth for every task above.
- `scripts/install-aliases.sh` — invoked by `task aliases` and
  auto-invoked by `task install` (idempotent fenced-block writer).
- `scripts/local-ci.sh` — invoked by `task local-ci`.
- `scripts/prep-pr.sh` — invoked by `task prep-pr`.
- `scripts/check-deps.sh` — invoked by `task check-deps`.
- `scripts/version.sh` — sourced by `CRABCC_VERSION` and `task version`.
