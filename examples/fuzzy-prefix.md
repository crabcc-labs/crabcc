# `crabcc fuzzy` & `crabcc prefix` — native name search

For when you don't remember the exact name. Built in-memory from the live
`.crabcc/index.db` symbol table on each call (no sidecar), so results always
reflect the current index.

## Fuzzy match (Levenshtein distance 2)

```bash
crabcc fuzzy Asseessment
```

```json
[{"name":"Assessment","kind":"class","file":"app/models/assessment.rb",
  "line":6,"parent":null,"score":1.0},
 {"name":"Assessable","kind":"module","file":"app/models/concerns/assessable.rb",
  "line":1,"parent":null,"score":0.5},
 …]
```

`score` is a synthetic closeness rank: `1.0` for an exact match, `0.5` at
distance 1, `0.33` at distance 2 — higher = closer.

Matching is **token-aware**: a name is split into alphanumeric segments, and the
query is matched against the whole name *or* any segment. So a typo in one part
of a `snake_case`/dotted name still matches (`usr` → `get_user_profile` via the
`user` token).

Common cases this catches:
- Typos: `Asseessment` → `Assessment` (Levenshtein 1).
- Plurals: `Assessments` → `Assessment` (Levenshtein 1).
- Transposed letters: `Aseessment` → `Assessment` (within the distance-2 cap).

Fails when the name is too far off — Levenshtein cap is 2. Use `prefix` instead.

## Prefix match

```bash
crabcc prefix getUser
```

```json
[{"name":"getUserKey","kind":"function","file":"src/auth/user.ts","line":42, …},
 {"name":"getUserAvatar","kind":"function","file":"src/avatar.ts","line":10, …},
 {"name":"getUserSession","kind":"method","parent":"AuthService", …}]
```

Case-insensitive starts-with, also token-aware — `crabcc prefix profile` finds
`get_user_profile` via the `profile` segment. Shortest matched unit ranks first.

## Custom limit

```bash
crabcc fuzzy logger --limit 5
crabcc prefix is_ --limit 50
```

Default limit: 20. When a short/common query matches a large slice of the corpus,
`fuzzy` fast-bails once it has filled the limit — so it returns in microseconds
rather than scanning every symbol.

## Freshness

Because fuzzy/prefix read the live `index.db`, they're never stale on their own —
there is nothing to re-sync. `crabcc fts-rebuild` still exists but is a **no-op**
kept for backward compatibility; it just reports the searchable symbol count:

```bash
crabcc fts-rebuild   # {"indexed":38214}
```

If results lag your edits, the index itself is stale — run `crabcc refresh` (or
`crabcc index`).

## When NOT to use

- You know the exact name → use `crabcc sym`.
- You want regex / partial substring (not just prefix) → there's no operator for that
  yet; fall back to `rg <regex>` to find call sites by text, then `crabcc sym` once
  you have a candidate name.
- Your repo isn't indexed → run `crabcc index` first.
