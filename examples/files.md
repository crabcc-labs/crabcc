# `crabcc files` — list indexed files

> Replaces `ls -R` / `find . -name '*.rb'` / `rg --files -g '*.rb'` for code files.

Backed by the SQLite store — no disk walk per query, no `node_modules` filtering
to remember.

## All indexed files

```bash
crabcc files
# ["a.ts","app/models/user.rb","app/models/assessment.rb", …]
```

## By extension

```bash
crabcc files --ext rb
crabcc files --ext ts
```

## By language

```bash
crabcc files --lang ruby
crabcc files --lang typescript
crabcc files --lang javascript
crabcc files --lang tsx
```

## Under a path prefix

```bash
crabcc files --under app/models
crabcc files --under app/services/exports --ext rb
```

## With a cap

```bash
crabcc files --ext rb --limit 5
# ["app/models/ab_relationship.rb","app/models/abid.rb","app/models/abid_crosswalk.rb",
#  "app/models/access_token.rb","app/models/account.rb"]
```

## Bench (mc-mothership)

| Tool                                          | Bytes    | Time      |
|-----------------------------------------------|---------:|----------:|
| `crabcc files --ext rb`                       | 244 KB   | **14 ms** |
| `rg --files -g '*.rb'`                        | 234 KB   | 65 ms     |
| `find . -type f -name '*.rb' …`               | 1.9 MB   | 10.4 s    |

crabcc and ripgrep both respect `.gitignore` — `find` doesn't. The `find` output
includes 1.9 MB of `node_modules/`/`tmp/`/`.git/` paths that no agent should see.

## Why prefer over `rg --files`

`rg --files` is fine. crabcc's edge:

- 4–5× faster (SQL vs disk walk).
- Deterministic ordering (sorted alphabetically).
- Filter by **language**, not just extension — `crabcc files --lang typescript`
  picks up TS without remembering whether you mean `.ts`, `.tsx`, or both.
