# `crabcc refs` — find all identifier references

> Replaces `rg '\bAssessment\b'` / `grep -rn 'Assessment'`

Returns every identifier whose text equals `<name>` across all indexed files —
definition, imports, type annotations, usages.

## Full hit list (no flags)

```bash
crabcc refs Assessment
```

```json
[{"file":"app/models/assessment.rb","line":6,"col":7,"snippet":"class Assessment < ApplicationRecord"},
 {"file":"app/models/assessment.rb","line":42,"col":12,"snippet":"belongs_to :assessment"},
 …]
```

Each hit: `{file, line, col, snippet}`. Snippet is the trimmed line, capped at 80 chars.

**Cost on mc-mothership:** 446 hits → 62,541 bytes (~15.6k tokens). For most "yes/no"
or "which files?" questions, that's wildly more than you need — use the shaping flags.

## `--count` — how many references?

```bash
crabcc refs Assessment --count
# {"count":446}
```

**14 bytes total** (~3 tokens). Use whenever the question is "how many?" — never
make the agent count by reading lines.

## `--files-only` — which files reference it?

```bash
crabcc refs Assessment --files-only --limit 10
```

```json
{"files":["app/builders/materials/part_builder.rb",
          "app/builders/materials/single_material_builder.rb",
          "app/builders/materials/score_type_builder.rb", …]}
```

Deduped per file, no per-line payload. **88% smaller than the full hit list.**

Drop `--limit` to get every file (still much smaller than the full hits).

## `--limit N` — sample of hits

```bash
crabcc refs Assessment --limit 5
```

Same shape as the unflagged version, but caps at 5 hits and **early-stops the
per-file walk** — it does not load the rest of the repo.

## Choosing a mode

| Question                              | Flag                                    |
|---------------------------------------|-----------------------------------------|
| "How many references to X?"           | `--count`                               |
| "Which files reference X?"            | `--files-only` (+ optional `--limit`)   |
| "Show me a few examples"              | `--limit 5`                             |
| "I need every reference for refactor" | (no flags)                              |

## Bench

| Mode                                          | Bytes      | Time          |
|-----------------------------------------------|-----------:|--------------:|
| `crabcc refs Assessment`                      | 62,541     | ~80 ms        |
| `crabcc refs Assessment --files-only --limit 10` | 513      | **38 ms**     |
| `crabcc refs Assessment --files-only`         | 7,759      | 70 ms         |
| `crabcc refs Assessment --count`              | 14         | ~30 ms        |
| `rg -l '\bAssessment\b' \| head -10`          | 460        | 14 s          |
| `grep -rlE '\bAssessment\b' …`                | 460        | TIMEOUT (60s) |

The `--files-only` and `--count` modes are **early-stop** — they short-circuit per-file.
