# `crabcc sym` — find a symbol by exact name

> Replaces `grep -rnE 'class Foo|module Foo|def Foo' --include='*.rb'`

## Basic

```bash
crabcc sym Assessment
```

```json
[{"name":"Assessment","kind":"class","signature":"class Assessment < ApplicationRecord",
  "parent":null,"file":"app/models/assessment.rb","line_start":6,"line_end":991,
  "visibility":null}]
```

One JSON object per definition. Includes:

| Field          | Meaning                                                         |
|----------------|-----------------------------------------------------------------|
| `name`         | Identifier name (case-sensitive exact match).                   |
| `kind`         | `function` / `method` / `class` / `struct` / `enum` / `trait` …|
| `signature`    | The declaration line (no body).                                 |
| `parent`       | Enclosing class/module/namespace, or `null`.                    |
| `file`         | Repo-relative path.                                             |
| `line_start`   | First line of the definition (1-based).                         |
| `line_end`     | Last line (inclusive).                                          |
| `visibility`   | `public` / `private` / language-specific, or `null` if unknown.|

## Multiple definitions

When the same name appears in several places (a common Rails pattern), all are returned:

```bash
crabcc sym Assessment
```

```json
[
  {"name":"Assessment","kind":"class","file":"app/models/assessment.rb","line_start":6,…},
  {"name":"Assessment","kind":"class","parent":"Headers",
   "file":"app/services/exports/v2/headers/assessment.rb","line_start":4,…},
  {"name":"Assessment","kind":"class","parent":"Data",
   "file":"app/services/exports/v2/data/assessment.rb","line_start":4,…}
]
```

Filter by `parent` or `file` with `jq` if you need just one — see
[`jq-pipelines.md`](./jq-pipelines.md).

## Reading the body

`sym` returns line ranges, not bodies. To read just the relevant range:

```bash
crabcc sym Assessment | jq -r '.[0] | "\(.file) lines \(.line_start)-\(.line_end)"'
# app/models/assessment.rb lines 6-991
```

Then `Read app/models/assessment.rb 6 991` (or `sed -n '6,991p' app/models/assessment.rb`).
**Don't** `Read` the whole file when `sym` already gave you the range.

## Bench (mc-mothership, ~13k files)

| Tool                      | Bytes        | Time       |
|---------------------------|-------------:|-----------:|
| `crabcc sym User`         | 1.2k         | **68 ms**  |
| `rg 'class User\b\|module User\b' …` | 201          | 732 ms     |
| `grep -rnE 'class User\b\|module User\b' …` | TIMEOUT      | 60 s ⚠     |

crabcc is **~10× faster than ripgrep** here (SQL lookup vs disk scan), and emits
structured JSON instead of raw `file:line:contents` strings.
