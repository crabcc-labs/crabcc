# `crabcc fuzzy` & `crabcc prefix` — Tantivy-backed name search

For when you don't remember the exact name. Backed by a Tantivy sidecar at
`.crabcc/tantivy/`, rebuilt automatically on `crabcc index`.

## Fuzzy match (Levenshtein distance 2)

```bash
crabcc fuzzy Asseessment
```

```json
[{"name":"Assessment","kind":"class","file":"app/models/assessment.rb",
  "line":6,"parent":null,"score":1.42},
 {"name":"Assessable","kind":"module","file":"app/models/concerns/assessable.rb",
  "line":1,"parent":null,"score":0.97},
 …]
```

`score` is Tantivy's BM25-style relevance — higher = closer match.

Common cases this catches:
- Typos: `Asseessment` → `Assessment` (Levenshtein 1).
- Plurals: `Assessments` → `Assessment` (Levenshtein 1).
- Transposed letters: `Aseessment` → `Assessment`.

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

Case-insensitive starts-with, backed by `RegexQuery` over the Tantivy `name` field
(needed because Tantivy's `QueryParser` wildcard doesn't work with tokenized TEXT
fields — see `crates/crabcc-core/src/fts.rs`).

## Custom limit

```bash
crabcc fuzzy logger --limit 5
crabcc prefix is_ --limit 50
```

Default limit: 20.

## Re-syncing the sidecar

The Tantivy sidecar is rebuilt automatically by `crabcc index`. `crabcc refresh`
deliberately does **not** rebuild it — the SQLite index is fast to refresh, but
Tantivy is a few seconds per rebuild. If your fuzzy/prefix results lag the SQLite
index after many refreshes:

```bash
crabcc fts-rebuild
```

Output: `{"indexed":38214}` — the symbol count just rebuilt.

## When NOT to use

- You know the exact name → use `crabcc sym`.
- You want regex / partial substring (not just prefix) → there's no operator for that
  yet; fall back to `rg <regex>` to find call sites by text, then `crabcc sym` once
  you have a candidate name.
- Your repo isn't indexed → run `crabcc index` first.
