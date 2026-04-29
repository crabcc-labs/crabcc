# crabcc CLI — examples

> Symbol-aware code lookups for AI agents. Returns compact JSON, not file dumps.
> **Up to 4400× faster than `grep -rn`** on monorepos. **88–99% fewer bytes** when shaped right.

All examples assume:

```bash
crabcc index   # one-time build, ~5–30s on a 13k-file repo
```

Re-run `crabcc refresh` after edits — it's incremental (mtime + sha256), takes ~250ms on a no-op.

---

## 1. Find a symbol — `crabcc sym`

> Replaces `grep -rn 'class Foo\|def Foo' --include='*.rb'`.

```bash
crabcc sym Assessment
```

```json
[{"name":"Assessment","kind":"class","signature":"class Assessment < ApplicationRecord",
  "parent":null,"file":"app/models/assessment.rb","line_start":1,"line_end":991,
  "visibility":null}]
```

**One JSON object per definition.** Includes the file path, line range, parent class, and the actual signature. Read just the line range — you don't need the whole file.

---

## 2. Find references — `crabcc refs`

> Replaces `grep -rn 'Assessment'`.

```bash
crabcc refs Assessment
```

Returns every identifier reference (definition + every usage) across the repo as `[{file,line,col,snippet},…]`.

### Token-shaping flags

When you only need *which files* — not *every line* — drop the snippet payload:

```bash
crabcc refs Assessment --files-only --limit 10
# {"files":["app/builders/materials/part_builder.rb", … (10 paths)]}
# 253 bytes  vs.  62,541 bytes  (–99.6%)
```

When you only need *how many*:

```bash
crabcc refs Assessment --count
# {"count":446}
# 14 bytes  vs.  62,541 bytes  (–99.98%)
```

Cap a full hit list:

```bash
crabcc refs Assessment --limit 5
```

---

## 3. Find call sites — `crabcc callers`

> Replaces `grep -rnE '\bfoo\(|\.foo\('`.

Catches **both** bare calls (`find_by(...)`) and method-receiver calls (`User.find_by(...)`):

```bash
crabcc callers find_by --count
# {"count":475}
```

```bash
crabcc callers find_by --files-only --limit 20
# 821 bytes
```

```bash
crabcc callers find_by --limit 3
# [{"file":"app/models/user.rb","line":42,"col":12,"snippet":"User.find_by(email: …"}, …]
```

---

## 4. File outline — `crabcc outline`

> Replaces `Read app/models/assessment.rb` for "what's in this file?" questions.

```bash
crabcc outline app/models/assessment.rb
```

Returns every symbol in the file, ordered by line. **991-line model file → ~17 KB JSON, no method bodies.** Reading the file would cost ~30 KB. Use the `line_start`/`line_end` ranges to selectively `Read` only the methods you need.

---

## 5. List indexed files — `crabcc files`

> Replaces `find . -name '*.rb' -not -path './node_modules/*' -not -path './tmp/*' …`.

```bash
crabcc files --ext rb
# 243 KB  vs.  find = 1.9 MB  (–87% — gitignore-aware by default)
# 600× faster, since crabcc never walks node_modules/tmp/.git
```

Filter by language, prefix path, or extension:

```bash
crabcc files --lang ruby --under app/models --limit 5
# ["app/models/ab_relationship.rb", "app/models/abid.rb", …]
```

---

## 6. Fuzzy + prefix search — `crabcc fuzzy` / `crabcc prefix`

> When you misremember a name. Backed by a Tantivy sidecar.

```bash
crabcc fuzzy Asseessment    # Levenshtein distance 2 — finds "Assessment"
crabcc prefix getUser       # case-insensitive starts-with — finds getUserKey, getUserAvatar, …
```

Both return `[{name, kind, file, line, parent, score}, …]`.

---

## 7. Re-indexing

```bash
crabcc index      # full rebuild (~5–30s)
crabcc refresh    # incremental — mtime + sha256 diff (~250ms no-op on 13k files)
crabcc fts-rebuild  # rebuild Tantivy fuzzy/prefix sidecar only
```

`crabcc index` rebuilds Tantivy too. `crabcc refresh` does not — run `fts-rebuild` if fuzzy results lag your edits.

---

## 8. Token-savings tracker — `crabcc track`

```bash
crabcc track
```

```
crabcc usage:
  session       4 queries     1,041 tokens used      28,452 saved
  last 24h     38 queries    12,003 tokens used     321,997 saved
  all-time    102 queries    32,540 tokens used     897,310 saved

by operation:
  callers       6 queries     1,820 tokens used      33,140 saved
  files         8 queries     2,400 tokens used      19,200 saved
  outline       3 queries     7,500 tokens used      10,500 saved
  refs         11 queries       420 tokens used      99,180 saved
  sym          74 queries    20,400 tokens used     735,290 saved
```

Saved-token math is heuristic — it estimates what `grep + Read` would have cost in the agent's context window.

---

## Pair with `jq` for advanced shaping

crabcc emits compact JSON; `jq` projects it into exactly what you need.

```bash
# Just the file paths
crabcc refs Assessment | jq -r '.[].file' | sort -u

# Symbol name + line, tab-separated
crabcc outline app/models/user.rb | jq -r '.[] | [.name, .line_start] | @tsv'

# All public methods in a class
crabcc outline foo.rb | jq '[.[] | select(.kind=="method" and .visibility!="private")]'

# Callers grouped by file with a count
crabcc callers find_by | jq 'group_by(.file) | map({file: .[0].file, n: length})'

# Top 5 files with most refs to X
crabcc refs Foo | jq 'group_by(.file) | sort_by(-length) | .[:5] | map({file: .[0].file, n: length})'
```

Use `jq -r` to drop JSON quoting when piping to another tool or showing the user a clean list.

---

## When NOT to use crabcc — fall back to `rg`/`fd`

| Situation                                     | Reach for                                |
|-----------------------------------------------|------------------------------------------|
| Free-text in markdown / yaml / json / configs | `rg "pattern" path/`                     |
| Need full function bodies                     | `crabcc sym X`, then `Read` line range   |
| Filename glob / age / non-code files          | `fd PATTERN path/`                       |
| Repo isn't indexed (`.crabcc/index.db` gone)  | run `crabcc index`, or `rg`/`fd` for now |
| Single small file < 200 lines, raw lines      | `rg -n pattern file` is already cheap    |

**Never use `grep -rn` or `find . -name` on a real repo** — they walk `node_modules/`,
`.git/`, `tmp/`. `rg` and `fd` are gitignore-aware by default, like crabcc.

---

## Cheatsheet

| Question                       | Command                                               |
|--------------------------------|-------------------------------------------------------|
| Where is `Foo` defined?        | `crabcc sym Foo`                                      |
| What calls `handleAuth`?       | `crabcc callers handleAuth`                           |
| Just how many?                 | `crabcc callers handleAuth --count`                   |
| Just which files?              | `crabcc callers handleAuth --files-only --limit 20`   |
| All references to `UserId`     | `crabcc refs UserId`                                  |
| What's in this file?           | `crabcc outline path/to/file.rs`                      |
| List all `.rb` files           | `crabcc files --ext rb`                               |
| Files under a directory        | `crabcc files --under app/models`                     |
| Misremembered name             | `crabcc fuzzy Authentcator`                           |
| Names starting with…           | `crabcc prefix getUser`                               |
| How much have I saved?         | `crabcc track`                                        |
