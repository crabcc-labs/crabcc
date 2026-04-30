# crabcc shell aliases

`scripts/install-aliases.sh` installs developer-friendly shell aliases for
modern CLI tools (rg, fd, bat, eza, dust, duf, procs, btop, zoxide, jq) and
crabcc-specific shortcuts. Each alias is gated on the modern binary being
on `PATH`, so a missing tool never breaks your rc file.

## Quick start

```bash
# Default (minimal): grep→rg, find→fd, cat→bat, ls→eza, plus cc/cci/ccs.
scripts/install-aliases.sh

# Aggressive: also wires short crabcc verbs into the global namespace
# so `sym Foo`, `refs Foo`, `callers Foo` all work without typing crabcc.
scripts/install-aliases.sh --aggressive

# Both shells at once (zsh + bash):
scripts/install-aliases.sh --aggressive --all-shells

# Preview without writing:
scripts/install-aliases.sh --aggressive --all-shells --dry-run

# Install all the modern tools first (brew on macOS, apt on Linux):
scripts/install-aliases.sh --install-tools
```

## Flags

| Flag                | Effect                                                   |
|---------------------|----------------------------------------------------------|
| *(none)*            | Install minimal aliases into the detected shell's rc.    |
| `--aggressive`      | Add crabcc verb aliases (`sym`, `refs`, `callers`, …).   |
| `--all-shells`      | Install into both `~/.zshrc` and `~/.bashrc`.            |
| `--shell <zsh\|bash\|fish>` | Force target shell.                              |
| `--dry-run`         | Print rc paths + block; do not modify any file.          |
| `--install-tools`   | `brew`/`apt` install missing modern tools first.         |
| `--print`           | Echo the alias block(s) only.                            |
| `--remove`          | Strip the fenced block from the rc file(s).              |

## Aggressive verbs (issue #81)

When you pass `--aggressive`, the script adds these short verbs (all gated
on `crabcc` being on `PATH`):

| Alias       | Expands to                       |
|-------------|----------------------------------|
| `gr`        | `crabcc grep`                    |
| `sym`       | `crabcc sym`                     |
| `refs`      | `crabcc refs --files-only`       |
| `callers`   | `crabcc callers --files-only`    |
| `outline`   | `crabcc outline`                 |
| `fuzzy`     | `crabcc fuzzy`                   |
| `diff`      | `delta` *(when delta installed)* |

Avoid `g` as the alias for grep — it clashes with the GNU coreutils `g`
macro on some Linux distros — hence `gr`.

## Idempotence

The script writes between two fence comments:

```
# >>> crabcc-aliases >>>
…
# <<< crabcc-aliases <<<
```

Re-running replaces the fenced block in place. `--remove` strips it. The
smoke test (`scripts/aliases-smoke.sh`) asserts that two consecutive
installs leave exactly one block.

## Pre-commit / CI hooks

Pair this with `scripts/install-hooks.sh`:

- **Local pre-commit (autofix)** runs `task pre-commit-fix`: `cargo fmt`,
  `clippy --fix`, and `aliases-smoke`.
- **CI fast-check** (in `.github/workflows/`) runs `task pre-commit-fast`:
  `fmt-check`, `clippy`, `aliases-smoke` — sub-15s.

## Smoke test

```bash
bash scripts/aliases-smoke.sh   # or: task aliases-smoke
```

Asserts:
1. Minimal mode emits gated grep/find/cat aliases.
2. Minimal mode does NOT emit aggressive verbs (back-compat).
3. `--aggressive` emits all crabcc verb aliases.
4. `--all-shells --dry-run` targets both zsh and bash.
5. Two consecutive installs leave exactly one fenced block.
