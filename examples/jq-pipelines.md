# Pairing crabcc with `jq`

crabcc emits compact JSON; `jq` reshapes it into exactly what your downstream step
needs. The pattern: **don't make the agent parse crabcc's output by hand** — pipe it.

## Basic projection

```bash
# Just the file paths from a refs query, deduped + sorted
crabcc refs Assessment | jq -r '.[].file' | sort -u
```

```bash
# Symbol name + line, tab-separated (good for grep-driven editor jumps)
crabcc outline app/models/user.rb | jq -r '.[] | [.name, .line_start] | @tsv'
```

```bash
# All public methods of a class
crabcc outline foo.rb | jq '[.[] | select(.kind=="method" and .visibility!="private")]'
```

## Filtering

```bash
# Methods longer than 50 lines (refactor candidates)
crabcc outline foo.rb | jq '[.[] | select(.kind=="method" and (.line_end - .line_start) > 50)]'
```

```bash
# Symbols whose signature mentions "deprecated"
crabcc outline foo.rb | jq '[.[] | select(.signature // "" | test("deprecated"; "i"))]'
```

## Grouping & counting

```bash
# Callers grouped by file with a count
crabcc callers find_by | jq 'group_by(.file) | map({file: .[0].file, n: length})'
```

```bash
# Top 5 files with most refs to X
crabcc refs Foo \
  | jq 'group_by(.file) | sort_by(-length) | .[:5]
        | map({file: .[0].file, n: length})'
```

## Combining queries

```bash
# Outline + filter to methods, then list each as `name → file:line`
crabcc outline app/models/user.rb \
  | jq -r '.[] | select(.kind == "method") | "\(.name) → \(.file):\(.line_start)"'
```

```bash
# Find every public method across every Ruby file in app/services/
for f in $(crabcc files --under app/services --ext rb); do
  crabcc outline "$f" \
    | jq -c --arg f "$f" '.[] | select(.kind=="method" and .visibility!="private") | {file: $f, name, line: .line_start}'
done
```

## Edits / scripted refactors

```bash
# Generate sed commands to rename Foo → Bar in every file that references it
crabcc refs Foo --files-only | jq -r '.files[]' \
  | xargs -I{} sed -i.bak 's/\bFoo\b/Bar/g' {}
# (Always crabcc refresh afterwards to update the index.)
```

## `--raw` vs structured

- `jq -r` (raw output) — outputs strings without JSON quotes. Use when piping to
  another shell tool or showing the user a clean list.
- `jq -c` (compact) — keeps JSON, one object per line. Use when feeding back into
  another `jq` or `crabcc` pipeline.
- `jq` (default pretty-print) — only when the user is reading directly.

## See also

- `man jq` or [jq tutorial](https://jqlang.github.io/jq/tutorial/)
- crabcc-shaping flags ([`refs.md`](./refs.md), [`callers.md`](./callers.md))
  do the simple cases (`--count`, `--files-only`) without needing jq.
- For complex cross-query analyses, jq is the right tool.
