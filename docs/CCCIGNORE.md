# `.cccignore` — controlling what crabcc indexes

crabcc walks your repo with gitignore-aware semantics. As of 5.1.0 it composes
**three** ignore sources, in increasing precedence:

1. `.gitignore` / `.ignore` (always honored; hidden dotfiles excluded too)
2. `.dockerignore` (so build-context excludes are skipped from the index)
3. **`.cccignore`** (crabcc-specific overrides; highest precedence)

All three use the same glob syntax as `.gitignore`. Because `.cccignore` is last,
it can **re-include** something the others excluded with a leading `!`.

## Why

Fewer files walked = faster `crabcc index`, smaller `.crabcc/index.db`, and a
faster load. Use `.cccignore` to drop things that are tracked in git (so
`.gitignore` won't hide them) but carry no useful symbols: vendored code,
generated bundles, large fixtures, binary/demo assets.

## Example

```gitignore
# .cccignore
target/
dist/
vendor/
*.min.js
assets/*.gif
testdata/huge-fixture/
!testdata/huge-fixture/keep-this.rs   # re-include one file
```

## Notes

- `.dockerignore` is parsed with gitignore semantics (close, not byte-identical
  to Docker's own parser) — fine for "don't index build artifacts".
- Python bytecode (`*.pyc`/`*.pyo`, `__pycache__/`) is always skipped regardless
  of ignore files.
- Run `crabcc index` after editing `.cccignore` to apply it (the FTS sidecar
  rebuilds on `index`, not `refresh`).
