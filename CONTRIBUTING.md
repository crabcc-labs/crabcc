# Contributing to crabcc

Short version: open an issue (use a [template](.github/ISSUE_TEMPLATE/)), branch off `main`, run `task ci` before pushing, open a PR. The full process below.

## Where to read first

| If you are… | Read |
|---|---|
| Human contributor | This file. |
| AI coding agent (any) | [`AGENTS.md`](AGENTS.md) |
| Claude Code specifically | [`CLAUDE.md`](CLAUDE.md) |
| Trying to understand the symbol-index internals | [`crates/crabcc-core/docs/HOW_IT_WORKS.md`](crates/crabcc-core/docs/HOW_IT_WORKS.md) |
| Curious about labels | [`.github/LABELS.md`](.github/LABELS.md) |

## Getting set up

```bash
# Local dev
git clone https://github.com/peterlodri-sec/crabcc
cd crabcc
task setup     # installs uv, Ollama, pulls models, starts the stack
task ci        # build + test + lint + fmt-check, in one shot
```

Or open the repo in **GitHub Codespaces** — `.devcontainer/` ships a Rust + Node + gh + docker-in-docker container with rust-analyzer, clippy on save, and ports 7878 (viz) / 8080 (litellm) forwarded. `cargo build` and `crabcc index` run automatically on `postCreate`.

### Nix dev shell (optional)

If you use Nix, `flake.nix` provides a devShell with the Rust toolchain plus the
[`.tools`](./.tools) CLI fleet (rg, fd, ast-grep, qsv, tokei, sccache, ...) and
`rtk` from [`numtide/llm-agents.nix`](https://github.com/numtide/llm-agents.nix):

```bash
nix develop          # drop into the shell
# or, with nix-direnv: `direnv allow` (auto-enters on cd; see .envrc)
```

The `nix` CI workflow runs `nix flake check` on every PR, so the shell stays
buildable. Agent runtimes (claude-code, the *claw bench profiles) live in the
same upstream flake: `nix run github:numtide/llm-agents.nix#<tool>`.

## Workflow

1. **Open an issue first** for anything non-trivial. Use one of the templates: bug, feature, performance, rfc, epic, chore. The template pre-fills the labels and the title prefix (`feat(scope):`, `perf(scope):`, etc.).
2. **Branch off `main`**. Branches are scoped: `feat/<short-desc>`, `fix/<short-desc>`, `chore/<short-desc>`. Do not pile unrelated commits onto an in-flight feature branch.
3. **Commit format** — Conventional Commits with the same scope as the issue title:
   ```
   feat(viz): add About view to /live
   fix(memory): drawer search panics on empty FTS5 result
   perf(desktop): box AppEvent telemetry variant
   ```
4. **Run `task ci` before pushing.** Same gate as CI.
5. **Open a PR** against `main`. The [PR template](.github/pull_request_template.md) asks for a Summary, What's in, Test plan, and a `Closes #N` link.
6. **Reviews** — small surgical PRs are reviewed in a day or two. Sweeping PRs ("refactor 8 crates") get pushed back to be split.

## Daily-driver commands

| Goal | Command |
|---|---|
| Build + test | `task` |
| Format gate | `task fmt-check` |
| Lint gate | `task lint` |
| Local CI dry-run | `task ci` |
| Symbol-index smoke | `task smoke` |
| Memory CLI smoke | `task memory-smoke` |
| Memory bench (R@5 gate) | `task memory-bench` |
| FSST gate bench | `task bench-compress REPO_FIXTURE=/path/to/big-repo` |
| Cut a release | `task release VERSION=x.y.z` |

The full target list lives in [`Taskfile.yml`](Taskfile.yml).

## Issue labels — quick reference

Every issue should carry **type + priority (+ milestone if scheduled)**. Templates pre-fill the type. The pairing convention from issues #236–#242 is the gold standard:

- `enhancement, feature, v3.0` — net-new capability
- `enhancement, performance, lang:rust` — perf work in Rust
- `enhancement, ci` — CI changes
- `enhancement, dependencies` — dep upgrades
- `enhancement, epic` — cross-cutting umbrella
- `enhancement, rfc` — design proposal

See [`.github/LABELS.md`](.github/LABELS.md) for the full taxonomy.

## Schema changes

Schema is **additive**. Never `DROP COLUMN`. Add a column + idempotent `ALTER` in `Store::open` (mirrored in `crabcc-memory/schema/`). See [`AGENTS.md`](AGENTS.md) for the full list of conventions agents must respect.

## Reporting security issues

Do **not** open a public issue for a security report. Use [GitHub Security Advisories](https://github.com/peterlodri-sec/crabcc/security/advisories/new). See [`SECURITY.md`](SECURITY.md).

## Code of conduct

Be respectful, be specific, ship code. We don't have a formal CoC document; if a situation arises that needs one, we'll add it.
