# Task 4 — extract.rs: split into two-pass walk, route uses through Resolver

## Context

v4.0 turns the extractor into a two-pass walk per file:

- **Pass 1** — walk every definition node, call `Store::insert_symbol`,
  collect the returned rowids into a `HashMap<String, SymbolId>` of local
  defs. This is essentially what the existing extractor's symbol-emission
  pass already does, but now it persists symbols one-by-one through the new
  `insert_symbol` API (which returns the rowid we need for pass 2) instead
  of batching through `replace_symbols`.
- **Pass 2** — walk every use / call site, build a `ScopeCtx`, ask the
  passed `&dyn Resolver` to translate the use-site name into a `SymbolId`.
  On `Some(id)` call `Store::insert_edge_resolved(src_id, id, kind, line)`.
  On `None` call `Store::upsert_unresolved_sentinel(name)` and emit an edge
  against the returned sentinel id.

The `Resolver` parameter is new on the per-file entry point. Existing callers
in `index.rs` (or wherever) must NOT be modified — they're outside the
allow-list. The pattern: add a new `extract_file_with_resolver(...)` function
that takes the resolver, and make the existing entry point a thin wrapper
that delegates with `&NameOnlyResolver` so backward compatibility holds for
free until per-language resolvers land in tasks 5–7.

## Required first step

**Before writing any code, read the current `crates/crabcc-core/src/extract.rs`
in full.** The structural change in this task is "split the existing walk
into two passes and add a resolver parameter" — it is NOT "rewrite the
extractor from scratch". You must preserve:

- The exact tree-sitter parser pool + language detection logic.
- The exact symbol-kind mapping (function / method / class / struct / etc.)
  per language.
- The exact set of node kinds the existing walker recognizes as definition
  sites and as use / call sites.
- The exact `signature`, `visibility`, `line_start`, `line_end` derivation
  for each definition.
- The existing public entry points (`extract_file_with_edges`,
  `extract_from_root`, and any others) — they must remain callable with
  their current signatures via thin wrappers that pass `&NameOnlyResolver`.

The change is **structural** (one walk becomes two, and use-sites route
through a `Resolver`), not **algorithmic** (no tree-sitter queries change,
no node-kind set grows or shrinks).

## What to change

File: `crates/crabcc-core/src/extract.rs` only.

### Step 1 — Imports

At the top of the file, add (next to the existing imports) whatever you need
of:

```rust
use crate::resolve::{ImportSpec, NameOnlyResolver, Resolver, ScopeCtx, SymbolId};
use std::collections::HashMap;
```

(`std::collections::HashMap` may already be imported — if so, don't duplicate
it.)

### Step 2 — Restructure the per-file entry point

The current per-file entry points are `extract_file_with_edges(...)` (around
line 124) and `extract_from_root(...)` (around line 160). For each one that
currently performs symbol + edge extraction against a `&Store`:

1. **Rename** the existing function to `<name>_with_resolver` (e.g.
   `extract_file_with_edges_with_resolver`). Add a `resolver: &dyn Resolver`
   parameter immediately after the `store: &Store` parameter. Keep every
   other parameter, generic bound, return type, and doc-comment in place.

2. **Reimplement** the body as two distinct passes:

   - **Pass 1 — definitions.** Walk the tree once, identifying every
     definition node the existing logic recognizes. For each one:
     - Compute `name`, `kind`, `parent_id` (resolve the enclosing
       class/impl/module's `SymbolId` from a stack you maintain during the
       walk, or `None` for top-level), `line_start`, `line_end`,
       `signature`, `visibility`, and `qualified` (the dotted/colon-joined
       path of enclosing modules + the name; pass `None` when the language
       doesn't make this trivially derivable from the AST).
     - Call `store.insert_symbol(file_id, name, qualified.as_deref(), kind,
       parent_id, line_start, line_end, signature.as_deref(),
       visibility.as_deref())?` to write the row and capture the returned
       `i64` rowid.
     - Insert `(name.to_string(), SymbolId(rowid))` into a
       `HashMap<String, SymbolId>` called `local_defs`. When the same name
       appears twice in the same file (overloaded methods, shadowing), the
       last write wins — that matches what `Store::find_by_name` would
       return for an ambiguous query anyway.
     - Also track, for each definition node, the rowid of the enclosing
       function/method/class so pass 2 can use it as `src_symbol_id` when
       it sees a use-site nested inside that definition.

   - **Pass 2 — uses / calls.** Walk the tree again. For each use-site or
     call-site the existing logic recognizes:
     - Determine the enclosing definition's `SymbolId` (the
       `src_symbol_id`). If a use-site appears at file scope outside any
       definition, skip it — v4 edges require a real source symbol.
     - Build a `ScopeCtx { file_id, current_module: <module path or None>,
       imports: <slice of ImportSpec collected from the file's import
       nodes>, local_defs: &local_defs }`.
     - For a call site, call `resolver.resolve_call(&scope, callee_name)`.
       For a reference site, call `resolver.resolve_ref(&scope, ref_name)`.
     - On `Some(SymbolId(dst_id))`, call
       `store.insert_edge_resolved(src_id, dst_id, kind_str, line)`.
     - On `None`, call `store.upsert_unresolved_sentinel(name)?` to get a
       sentinel id, then call `store.insert_edge_resolved(src_id,
       sentinel_id, kind_str, line)`. `kind_str` is `"call"` for call
       sites, `"ref"` for type/use references — the same mapping the
       current extractor already uses.

   - Collect the `imports: Vec<ImportSpec>` for the file once (either at
     the top of pass 2, or as a side-effect of pass 1 — your choice). For
     languages without straightforward import syntax (Ruby), pass an empty
     slice.

3. **Recreate the old entry point as a thin wrapper** that delegates with
   `&NameOnlyResolver`. The wrapper must keep the exact original signature
   so external callers (which the allow-list forbids modifying) continue to
   compile and link:

   ```rust
   pub fn extract_file_with_edges(
       /* ...original args, no resolver... */
   ) -> /* original return type */ {
       extract_file_with_edges_with_resolver(/* ...original args... */, &NameOnlyResolver)
   }
   ```

   Do the same for any other public entry point that previously triggered
   edge extraction.

### Step 3 — Remove direct calls to `Store::replace_edges`

The v4 `edges` table has no `src_file_id` / `dst_name` columns, so any
existing call to `store.replace_edges(...)` from inside `extract.rs` will
now insert against the wrong shape. Replace such calls with the per-edge
`insert_edge_resolved` path described above. (Pass 1 emits no edges; pass 2
is the only writer.)

If the existing code accumulates edges into a `Vec<Edge>` and then bulk-
writes them via `replace_edges`, replace the accumulator + bulk write with
direct `insert_edge_resolved` calls inside the pass-2 walk loop. Wrap the
whole `extract_file_with_edges_with_resolver` body in a single
`store.transaction(...)` only if the existing code already used a
transaction — otherwise keep the current write semantics.

### Step 4 — Keep tests compiling

If the existing `#[cfg(test)] mod tests` in `extract.rs` calls
`extract_file_with_edges` (or whatever the old entry point was), leave those
calls unchanged — they will now flow through the wrapper that supplies
`&NameOnlyResolver`. If a test asserts on edge `dst_name` strings (v3 shape),
update it to assert on resolved-vs-sentinel behaviour via
`store.upsert_unresolved_sentinel` lookups instead — but ONLY if the test
fails to compile under the new shape. Do not "improve" passing tests.

## Constraints

- **Allow-list is strict.** You may modify ONLY
  `crates/crabcc-core/src/extract.rs`. The plan-level wave dispatcher will
  diff against this allow-list and reject any other touched file. In
  particular: `index.rs`, `lib.rs`, `store.rs`, `resolve.rs`, and the
  schema file are off-limits in this task.
- **Preserve existing entry point signatures.** External callers in
  `crates/crabcc-core/src/index.rs` (and elsewhere) call
  `extract_file_with_edges` and `extract_from_root` directly. They must
  continue to compile without modification. The wrapper pattern in Step 2.3
  enforces this.
- **No new dependencies.** Use only what's already in `Cargo.toml`.
- **No "design as you go".** Pass 1 emits symbols, pass 2 emits edges.
  Pass 1 does NOT call the resolver. Pass 2 does NOT call `insert_symbol`.
  Keep the boundary clean — the next wave's per-language resolvers will
  rely on it.
- **Sentinel fallback is on every `None` from the resolver, no exceptions.**
  Even at file scope, even for imports, even for cross-file references —
  if `resolver.resolve_ref` or `resolver.resolve_call` returns `None`, the
  edge goes through `upsert_unresolved_sentinel`. The point is that
  `lookup refs Foo` still returns results for Ruby/Java/Swift after the
  v4 cutover.

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    refactor(extract)!: two-pass walk — pass 1 collects defs, pass 2 resolves uses via Resolver
