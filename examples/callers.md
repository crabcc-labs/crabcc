# `crabcc callers` — find call sites

> Replaces `rg '\bfind_by\(|\.find_by\('` / `grep -rnE '\bfind_by\(|\.find_by\('`

Catches **both** bare calls (`find_by(...)`) and method-receiver calls
(`User.find_by(...)`) by running two ast-grep patterns and unioning the hits:

- `name($$$)` — bare-receiver
- `$RECV.name($$$)` — method-receiver

## Full hit list

```bash
crabcc callers find_by
```

```json
[{"file":"app/models/user.rb","line":42,"col":12,"snippet":"User.find_by(email: …"},
 …]
```

Each hit: `{file, line, col, snippet}`.

## `--count` — how many call sites?

```bash
crabcc callers find_by --count
# {"count":475}
```

**14 bytes** vs ~16k tokens for the full list.

## `--files-only` — which files call it?

```bash
crabcc callers find_by --files-only --limit 20
```

```json
{"files":["app/admin/users.rb","app/builders/account_creator.rb", …]}
```

Useful for refactor planning: "which files do I need to update?"

## `--limit N` — sample

```bash
crabcc callers find_by --limit 3
```

Returns the first 3 hits, then early-stops the per-file walk.

## Cross-language behavior

- **TypeScript/JS**: catches `foo()`, `obj.foo()`, `obj?.foo()`, `(foo)()`, `cls.foo<T>()`.
- **Ruby**: catches `foo(...)`, `obj.foo(...)`, `Mod::Klass.foo(...)`. Does **not**
  catch chained-receiver calls like `obj.bar.foo` (v1 limitation — receiver must be
  a single token).

## Bench (mc-mothership)

| Mode                                       | Bytes  | Time      |
|--------------------------------------------|-------:|----------:|
| `crabcc callers find_by --count`           | 14     | **965 ms**|
| `crabcc callers find_by --files-only --limit 20` | 821 | 54 ms |
| `rg -oc '\bfind_by\(' \| awk …` (count)    | 4      | 701 ms    |
| `grep -rEoh '\bfind_by\(' … \| wc -l`      | 9      | 45 s      |

Note: ripgrep is competitive on `--count` (its tight regex on disk is fast). crabcc's
edge over rg comes from `--files-only` mode (deduped) and from the larger-result
queries where structured output saves agent context. Against `grep -rn`, crabcc wins
by 47×.

## When to fall back to `rg`

If you want every textual occurrence of `find_by` (including comments, strings,
`# TODO: find_by`), use `rg -n '\bfind_by\b'`. crabcc only counts call sites — it
ignores text-only mentions.
