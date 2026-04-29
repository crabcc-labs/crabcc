# `crabcc outline` — file structure without bodies

> Replaces `Read path/to/file.rb` for "what's in this file?" questions.

Returns every symbol in the file, ordered by line, **with line ranges, no method bodies**.
Use the ranges to selectively `Read` only the methods you care about.

## Basic

```bash
crabcc outline app/models/user.rb
```

```json
[{"name":"User","kind":"class","signature":"class User < ApplicationRecord",
  "parent":null,"file":"app/models/user.rb","line_start":1,"line_end":420,
  "visibility":null},
 {"name":"name","kind":"method","signature":"def name",
  "parent":"User","file":"app/models/user.rb","line_start":15,"line_end":17,
  "visibility":null},
 {"name":"email_verified?","kind":"method","signature":"def email_verified?",
  "parent":"User","file":"app/models/user.rb","line_start":42,"line_end":44,
  "visibility":null},
 …]
```

## Reconstructing the hierarchy

Use `parent` to group methods under their class/module:

```bash
crabcc outline app/models/user.rb | jq 'group_by(.parent) | map({class: .[0].parent, members: map(.name)})'
```

```json
[
  {"class":null,"members":["User"]},
  {"class":"User","members":["name","email_verified?","admin?","find_by_token", …]}
]
```

## Filtering by kind

```bash
# Just the public methods of User
crabcc outline app/models/user.rb | jq '[.[] | select(.kind=="method" and .visibility!="private")]'

# Just classes/modules (top-level structure)
crabcc outline app/models/user.rb | jq '[.[] | select(.kind=="class" or .kind=="module")]'
```

## When to use vs raw `Read`

| File size                | Recommendation                                           |
|--------------------------|----------------------------------------------------------|
| < 200 lines              | Just `Read` it — outline overhead isn't worth it.        |
| 200–500 lines            | Outline first if you only need a couple of methods.     |
| 500+ lines               | **Always outline first**, then `Read` the line range.   |
| 991-line `assessment.rb` | `outline` = ~17 KB; `Read` = ~32 KB (~50% saving).      |

## Bench

| Tool                            | Bytes    | Time   |
|---------------------------------|---------:|-------:|
| `crabcc outline assessment.rb`  | 17,381   | 11 ms  |
| `cat assessment.rb` (full Read) | 33,412   | 8 ms   |
| `rg -nE '^(class\|module\|def)…' assessment.rb` | 3,085 | 11 ms |

Honest tradeoff: `rg` on a single small file emits less but loses the structural
metadata (parent class, signature, kind, visibility). Pick `rg` if you only need
"what lines have `def`?" and pick `outline` if you need to navigate the class.
