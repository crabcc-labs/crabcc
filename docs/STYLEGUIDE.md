# crabcc Code Styleguide

## Rust

**Edition / MSRV:** Rust 2021, MSRV 1.86 (pinned in `clippy.toml`).

**Formatting:** `task fmt` (`cargo fmt`). No manual formatting tweaks; let rustfmt own it.

**Clippy:** CI treats warnings as errors. Fix the root cause; never `#[allow(...)]` to silence. Run `task lint` before pushing.

**Errors:**
- `anyhow::Result` at binary/CLI boundaries.
- `crabcc-core` and library crates return concrete `Result<T, ConcreteError>` only when callers branch on the variant.
- No `unwrap()` outside tests. Use `expect("why this can't fail")` when you're certain, so the message is actionable in crash logs.

**Naming:**
- Types: `PascalCase`. Functions/variables: `snake_case`. Constants: `SCREAMING_SNAKE_CASE`.
- Trait methods that return `Self`: prefer `new` / `from_*` / `with_*` over `build`/`make`.
- Boolean getters: `is_*` / `has_*`, not bare nouns.

**Comments:** Comment the *why*, not the *what*. One short line max. No docstrings added just to satisfy a linter. Don't reference callers or issue numbers in source comments — those belong in the PR description and rot.

**Schema:** Additive only. Never `ALTER TABLE … DROP COLUMN`. Add a column + an idempotent `ALTER` in `Store::open` (see how `signature_enc` landed). Same rule for `crates/crabcc-memory/schema/001_init.sql`.

**Tests:**
- `crabcc-core` must pass both `--features default` and `--no-default-features`.
- Write the failing test before the fix (red → green). If a regression test genuinely cannot be written, explain why in the PR description.
- Unit tests in the same file (`#[cfg(test)] mod tests {}`). Integration tests in `crates/*/tests/`.

**Lambda IR (`vaked-lambda`):**
- `Term` variants are the canonical IR — no ad-hoc string-based representations.
- Reduction passes implement `Reduce`; compose via `normalize()`.
- A term with no free `EnvVar` nodes is *closed* — `emit_mirage` can lower it to a constant OCaml expression.
- Open terms (residual env vars) become unikernel boot-config lookups.

---

## Amber

**File layout:** `scripts/amber/<name>.ab` (source) + `scripts/amber/<name>.sh` (compiled). Always commit both.

**Compilation:**
```bash
amber check scripts/amber/foo.ab
amber build scripts/amber/foo.ab scripts/amber/foo.sh --minify
```

**Style rules:**
- Use `enum` for all multi-value dispatch (visibility, outcome, backend). Never bare string constants in match arms.
- Use typed `fun` signatures everywhere — no untyped helpers.
- Parallelism: `trust $cmd &$` + `pid()` + `trust await([pids])`. Never `nohup` or fire-and-forget when you need the exit code.
- Use `unsafe $...$` only when invoking shell commands with no Amber equivalent; prefer `trust $...$` for commands that should propagate failure.
- Exit codes: capture via subshell `(cmd; echo $? > sentinel.exit)` pattern when the outer `await()` barrier needs per-job outcomes.

**Enum conventions:**
```amber
// Correct — enum for all dispatch
enum Vis { Public, Unlisted, Private, Direct }

// Wrong — bare strings
fun vis(): Text { return "public" }
```

---

## Shell (compiled `.sh` only)

Compiled `.sh` files are generated from Amber sources. Do not hand-edit them; changes will be overwritten on the next `amber build`. If you need a change in the shell output, edit the `.ab` source and recompile.

Exception: if `amber` is unavailable (CI runner without Amber installed), patch the `.sh` directly as a stopgap and open a follow-up to recompile from `.ab`.

---

## JavaScript (MV3 extension, `extensions/crabcc-devshell/`)

- Native messaging framing: 4-byte little-endian length prefix, then UTF-8 JSON.
- Service worker (`background.js`): ring buffer capped at 500 entries. No unbounded arrays.
- `chrome.runtime.connectNative`: reconnect on `onDisconnect`; log reason to ring buffer.
- No `eval`, no `innerHTML` string concatenation. CSP in `manifest.json` is strict-dynamic.

---

## Git

- One logical change per commit. No mixing feature work with formatting/refactor.
- Commit message: imperative mood, 72-char subject line, blank line, body explaining *why* (not *what*).
- Schema changes get their own commit.
- Never force-push `main`/`master`.
