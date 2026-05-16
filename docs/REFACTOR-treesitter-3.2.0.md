# Refactor plan — tree-sitter + 3.2.0 followups

> Branch base: `fix/cli-lookup-refs-followups` (commit `e236d6d` at time of
> writing — see "Branch state" below).
> Target tag: `v3.3.0` once tasks 1–6 land; tasks 7–8 are longer arc.
> Dispatch: each task below is a self-contained brief that can be pasted
> into one `agentfield/swe-*` agent without further context.

## Context

3.2.0 fixed two `lookup` bugs (Rust struct refs returning `[]`; qualified
names not matching) and introduced a `meta('ref_edges_built')` readiness
marker so stale pre-3.2 indexes auto-wipe-and-rebuild on next CLI use.
PR #547 was squash-merged.

The follow-up branch carries: the marker-key implementation
(`dded092`), `full_index` integration (`736867b`), populated-index gating
(`a272809`), and a CI overhaul that swaps in the `wild` linker + jemalloc
+ BuildKit cache mounts on Linux (`31baa19`, `5ceedf5`, `c7f5984`,
`daccb1b`, `e236d6d`).

Outstanding gaps:

1. **Squash dropped a docs fix.** The `(schema v3)` parenthetical in the
   merge commit title made it into `CHANGELOG.md` even though the
   shipped implementation is the `ref_edges_built` meta key, not a
   `schema_version` bump. `crates/crabcc-core/docs/HOW_IT_WORKS.md` is
   similarly silent on the readiness-gate pattern.
2. **`extract.rs` is at the 1500-LoC ceiling** (1589 LoC) and is a
   per-language pipeline that fans out to giant `match lang { … }`
   ladders inside generic helpers. Splitting per-language is the
   natural cut.
3. **Language dispatch is duplicated.** `extract::ts_language` and
   `refs::find_refs` both maintain a parallel `match lang →
   tree_sitter_*::LANGUAGE` table; one is private. The public
   `extract::language()` API exists for exactly this case.
4. **MCP + LSP open `Store` without consulting `needs_reindex`.**
   `crates/crabcc-mcp/src/dispatch.rs:143` and
   `crates/ucracc-lsp/src/server.rs:65` both call `Store::open` and
   proceed. On a stale index they'll serve empty `lookup refs` results
   silently — the auto-wipe path is CLI-only.
5. **`refs::find_refs` is JS/TS/Ruby-only.** Now that the v3.2.0 fix
   wired Rust ref-edges into the `edges` table, the streaming
   identifier scan is only used as a fallback. Either fold it into
   `query::find_refs` consistently or document the routing.
6. **Tree-sitter version split.** Workspace pins `tree-sitter = "0.26"`
   plus 7 grammar crates at matching versions; `crates/crabcc-desktop`
   is workspace-excluded *specifically* because `gpui-component` hard-
   depends on `tree-sitter = "0.25"` with `links = "tree-sitter"`.
   Check whether gpui-component has caught up (release notes from
   2026-04 onward) and re-join the desktop crate to the workspace if
   so.
7. **Per-language-trait refactor.** The big-bang version of (2):
   instead of splitting the file into per-language modules of free
   functions, introduce a `trait LanguageWalker` (one impl per
   language) so adding a language is a single module + one row in a
   registry. Defer to v3.4 unless (2) reveals the trait shape clearly.
8. **Cross-language ref-edge parity.** v3.2 fixed Rust. Go, Python,
   TS/JS, Java, Swift, Bash, Ruby all extract symbols but emit no
   `kind=ref` edges. `lookup refs Foo` only works for Rust.

## Branch state

```
e236d6d fix(ci): clear wild RUSTFLAGS while building wild itself
daccb1b chore: sync Cargo.lock to workspace 3.2.0
c7f5984 fix(ci): select wild-linker package on cargo install from git
736867b fix(index): mark ref_edges_built inside full_index, not just auto_index
5ceedf5 fix(ci): install wild from git — crates.io publishes it as library only
31baa19 ci: wild linker + CC=cc + jemalloc on Linux; BuildKit cache mounts in Dockerfile
a272809 fix(store): gate needs_reindex on populated index; fix auto_index + CI
dded092 fix(store): use ref_edges_built key instead of schema_version for reindex detection
2a0b959 fix(lookup): refs/callers now work for Rust structs + qualified names (schema v3) (#547)
```

---

## Task dependency graph

```
T1 (docs) ── independent, ship first
T2 (extract.rs split) ── independent
T3 (ts_language dedup) ──── depends on T2 done (so the call sites move
                                              with their language module)
T4 (MCP/LSP stale-handling) ── independent
T5 (refs.rs routing audit) ── independent; light coupling to T8
T6 (tree-sitter version unify) ── independent, blocks desktop work
T7 (LanguageWalker trait)  ──── depends on T2
T8 (cross-language ref-edges) ── depends on T2 (or T7 if it ships first)
```

Recommended dispatch order: **T1 + T4 + T6 in parallel** (different files,
no overlap). Then **T2**. Then **T3 + T5 in parallel**. Then **T7 or T8**.

---

## T1 — `swe-docs` (or any swe-*) — re-apply the dropped docs fix

**Branch from:** `fix/cli-lookup-refs-followups`
**Output PR base:** `main`
**Target files:** `CHANGELOG.md`, `crates/crabcc-core/docs/HOW_IT_WORKS.md`
**Size:** ~30 lines of diff
**Acceptance:** `task fmt-check` + `task lint` green; no code changes.

### Problem

The PR #547 squash-merge took the pre-correction CHANGELOG text:

```
- Indexes built before v3.2.0 are automatically detected on open via
  `schema_version` (bumped from 2 to 3). A stale index is wiped and rebuilt
  transparently before serving the first command, including an FTS sidecar
  rebuild.
```

This is wrong on the mechanism. The shipped implementation uses a
separate `meta('ref_edges_built')` key, written only by
`Store::mark_ref_edges_built()` after `full_index` completes
(`crates/crabcc-core/src/store.rs` and the `IndexOp::Build` call site
in `crates/crabcc-cli/src/main.rs`). `schema_version` exists but is
forensic-only — bumping it would have re-introduced the bug Codex
flagged on PR #547 (non-CLI openers persisting the new version on
stale data).

### Brief

1. Rewrite the CHANGELOG bullet (currently `CHANGELOG.md:22-25`) to
   describe the `ref_edges_built` readiness-gate mechanism.
2. In `crates/crabcc-core/docs/HOW_IT_WORKS.md`:
   - Line listing the `meta` table contents (currently `schema_version`,
     `edges_populated`) needs `ref_edges_built` added.
   - "Schema discipline" subsection needs a new bullet explaining why
     data-readiness gates are *separate* keys from `schema_version`
     (MCP and LSP open `Store` first — bumping the version stamp on
     stale rows would hide staleness from every subsequent opener).
   - "Evolve the schema" recipe needs a follow-on rule: if a migration
     requires *row contents* to be rebuilt (not just a column shape
     change), add a dedicated `meta` key written only post-rebuild
     and read into `Store::needs_reindex`.

### Reference commit

A prior session shipped this exact change as
`575f428 docs: explain ref_edges_built readiness-gate pattern` on the
sister branch `fix/cli-lookup-refs-callers` (now merged), but it was
*not* retained by the squash. The diff is small enough to rewrite from
scratch — don't bother cherry-picking.

### Verification

```bash
grep -n "ref_edges_built" CHANGELOG.md crates/crabcc-core/docs/HOW_IT_WORKS.md
# Must return at least 1 hit per file. CHANGELOG must NOT still say
# "bumped from 2 to 3".

task fmt-check && task lint
```

---

## T2 — `swe-refactor` — split `extract.rs` into per-language modules

**Branch from:** `fix/cli-lookup-refs-followups`
**Output PR base:** `main`
**Target files:** `crates/crabcc-core/src/extract.rs` (1589 LoC) →
`crates/crabcc-core/src/extract/{mod,common,lang/<lang>.rs}` tree
**Size:** medium-to-large; touches a contained file
**Acceptance:** `task` (build + test workspace), no public-API break

### Problem

`extract.rs` carries the parser pool, language detection, the symbol
walker, the edge walker, the per-language `is_callable`/`call_target`/
`ref_target`/`symbol_kind_for`/`signature_for`/`visibility_for`
match-ladders, *and* the test module. At 1589 LoC it's now over the
informal 1500 ceiling we set during 3.1.0 modularization (see PRs
#537 / #538 / #546). Per-language ladders are also the natural
cohesion boundary — when adding a new language the diff touches
every ladder.

### Brief

Split into the following tree, preserving every public symbol:

```
crates/crabcc-core/src/extract/
├── mod.rs              # public surface: detect_lang, language, extract_file,
│                       #   extract_file_with_edges, extract_from_root,
│                       #   the parser pool, the generic walk + walk_edges
│                       #   that dispatch into the lang modules.
├── common.rs           # node_name, strip_generics, go_receiver_type, the
│                       #   `Bump` arena constant, anything language-agnostic
│                       #   that's shared by ≥2 lang modules.
└── lang/
    ├── mod.rs          # `pub(super) use` re-exports for the dispatcher.
    ├── rust.rs         # is_callable, call_target, ref_target,
    │                   #   symbol_kind_for, signature_for, visibility_for
    │                   #   — Rust arms only.
    ├── typescript.rs   # TS + TSX share most logic; one file, two `LANGUAGE_*`
    │                   #   entries in mod.rs's lookup.
    ├── javascript.rs
    ├── ruby.rs
    ├── go.rs
    ├── python.rs
    ├── swift.rs
    ├── bash.rs
    └── java.rs
```

Each `lang/<L>.rs` exposes the same six fns as `pub(super) fn`
(callable from `extract::mod` only). The top-level `is_callable` etc.
in `mod.rs` becomes a single `match lang { "rust" => rust::is_callable(kind),
… }` — no per-fn match ladder anymore.

**Constraints:**
- `detect_lang`, `language`, `extract_file`, `extract_file_with_edges`,
  `extract_from_root` must keep their current signatures and remain
  `pub` from the crate. Tests in `crates/ucracc-lsp/`,
  `crates/crabcc-cli/`, `crates/crabcc-mcp/` import via these names —
  audit them with `grep -rn "extract::" crates/` before touching.
- The parser pool's `thread_local!` map stays in `mod.rs`. Don't try
  to push it down — it's keyed on the `&'static str` lang tag the
  dispatcher already owns.
- Tests (currently in the bottom third of `extract.rs`) move into
  per-language modules if they're language-scoped, or stay in `mod.rs`
  if they cross multiple languages (e.g. `extract_edges_unsupported_lang_errors`).
- `Bump` scratch arena lives wherever the walker that uses it goes —
  today that's a no-op (`let _scratch = Bump::with_capacity(…)`), but
  the constant `SCRATCH_ARENA_BYTES` and the allocation should ride
  along together.

**Out of scope** (defer to T7): introducing a `trait LanguageWalker`.
This task is a *mechanical* split that preserves the free-function
shape; T7 is the optional follow-up.

### Verification

```bash
task              # full build + test
task smoke        # CLI smoke
cargo doc --no-deps -p crabcc-core  # rustdoc still resolves all links
git diff --stat   # mod.rs + 10 new files; no diff in callers
```

LoC budget check (informal):
- `mod.rs` ≤ 600 LoC
- `common.rs` ≤ 150 LoC
- each `lang/<L>.rs` ≤ 250 LoC
- total ≤ 1700 LoC (small overhead from new module headers)

---

## T3 — `swe-refactor` — fold `refs::find_refs` language dispatch into `extract::language()`

**Branch from:** `fix/cli-lookup-refs-followups` (or after T2 lands)
**Output PR base:** `main`
**Target files:** `crates/crabcc-core/src/refs.rs`
**Size:** small (~30 lines deleted, single import added)
**Acceptance:** `cargo test -p crabcc-core` green; `refs.rs` no longer
matches `tree_sitter_<lang>::LANGUAGE` directly.

### Problem

`crates/crabcc-core/src/refs.rs:11-31` opens with its own
`match lang { "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), … }`
ladder — a direct copy of `extract::ts_language` (private) and
`extract::language` (public). Three places to update when a grammar
crate's API changes; two when adding a language.

### Brief

1. `extract::language(lang: &str) -> Result<tree_sitter::Language>` is
   already public (see `crates/crabcc-core/docs/HOW_IT_WORKS.md:292`).
2. Replace the inline match in `refs::find_refs` with
   `let language = crate::extract::language(lang)?;`.
3. Keep the JS/TS/Ruby gate that follows — `find_refs` is deliberately
   narrower than the full set of supported languages because it's the
   streaming fallback path. The gate stays in `refs.rs` as
   `is_identifier_kind` continues to be lang-scoped.
4. Delete the now-unused per-grammar `use tree_sitter_*` lines from
   `refs.rs` if any are no longer referenced (they likely are still
   needed for `is_identifier_kind` — leave those that are).
5. If T2 has not landed yet, also factor out the duplicate from
   `extract::ts_language` (which currently has the *same* ladder, just
   private). Then T2 will inherit one less duplication to clean up.

### Verification

```bash
cargo test -p crabcc-core --lib 'refs::tests'
grep -n "tree_sitter_[a-z]*::LANGUAGE" crates/crabcc-core/src/refs.rs
# After the change the only hits should be inside is_identifier_kind,
# not in find_refs's setup.
```

---

## T4 — `swe-feature` — MCP + LSP must act on `Store::needs_reindex`

**Branch from:** `fix/cli-lookup-refs-followups`
**Output PR base:** `main`
**Target files:**
- `crates/crabcc-mcp/src/dispatch.rs:143` (the `Store::open` call site)
- `crates/ucracc-lsp/src/server.rs:65` (the `match Store::open` arm)
- `crates/crabcc-core/src/store.rs` (potentially add a helper)
**Size:** medium
**Acceptance:** new integration test on each side that opens a stale
DB and asserts the right behaviour; `task` + `task smoke` green.

### Problem

The 3.2.0 auto-wipe path lives in `crates/crabcc-cli/src/main.rs:1117`.
MCP and LSP also open `Store`, see `needs_reindex == true`, and **do
nothing about it** — they serve queries against a half-rebuilt
(or unbuilt) edges table and return empty `lookup refs` to the caller
with no signal that the index is stale.

This is a load-bearing UX bug: someone running `claude-desktop` or
`ucracc-lsp` over a pre-3.2.0 repo will see refs/callers silently
returning `[]` until they happen to run a CLI command.

### Brief

Two acceptable resolutions; pick one and apply consistently across
both consumers.

**Option A (recommended): consumers run the wipe-and-rebuild.**
Extract the CLI's wipe-and-rebuild block at `main.rs:1117-1131` into
`crate::store::wipe_and_rebuild(db: &Path, root: &Path, fts_dir: &Path,
compress: bool) -> Result<Store>`. Call it from MCP `dispatch.rs:143`
and LSP `server.rs:65` when `needs_reindex` is true. MCP can do this
synchronously (it's a short-lived per-request worker). LSP should do
it on a background thread and refuse `textDocument/references` /
`callHierarchy/incomingCalls` requests with a `ResponseError` until
the rebuild completes; `window/logMessage` the progress for client
visibility.

**Option B: consumers refuse to serve until the CLI rebuilds.**
Return a typed `SamplingError`-equivalent on MCP and an LSP
`InternalError` with `data.code = "stale_index"` on LSP. Less
surprising than silent empty results; less friendly than auto-fix.

Either way, add:

1. An integration test on the MCP side
   (`crates/crabcc-mcp/tests/<new>.rs`) that builds a stale DB
   (force-delete the `ref_edges_built` row), calls the `lookup.refs`
   tool, and asserts either rebuild-then-result (A) or typed-error
   (B).
2. An integration test on the LSP side
   (`crates/ucracc-lsp/tests/integration.rs` likely already has the
   stub) covering the same scenario.

### Verification

```bash
cargo test -p crabcc-mcp --tests
cargo test -p ucracc-lsp --tests
task smoke
```

Also: after this lands, the redundant `auto_index::ensure_indexed`
call at `crates/crabcc-cli/src/main.rs:1144` should be re-examined —
if it now overlaps with the new shared helper, dedupe.

---

## T5 — `swe-research` — audit `query::find_refs` routing and document it

**Branch from:** `fix/cli-lookup-refs-followups`
**Output PR base:** `main`
**Target files:** `crates/crabcc-core/src/query.rs`,
`crates/crabcc-core/src/refs.rs`, `crates/crabcc-core/docs/HOW_IT_WORKS.md`
**Size:** mostly investigation + ~50 lines of docs/code
**Acceptance:** reader of `HOW_IT_WORKS.md` can answer "given a
`lookup refs` request, which code path runs?" without reading the
source.

### Problem

There are now two ref-finding paths:

1. **`edges` table** — populated by `extract::walk_edges` (Rust only
   today, will expand under T8). Queried by `query::find_refs` via a
   straight SELECT.
2. **Streaming identifier scan** — `refs::find_refs`, tree-sitter
   identifier walker over `file_contents`. JS/TS/Ruby only.

The 3.2.0 fix added a `query_refs falls back to query_callers for
unsupported languages` note in the CHANGELOG, but the actual dispatch
logic in `query::query_refs` (and the LSP's call site at
`ucracc-lsp/src/server.rs:393`) is hard to follow. There's no
single-source-of-truth document for which lang routes to which path.

### Brief

1. Read `query::query_refs` (likely in `crates/crabcc-core/src/query.rs`
   — find via `grep -n "query_refs\|find_refs" crates/crabcc-core/src/query.rs`)
   end-to-end.
2. Write down the decision tree. Likely shape:
   - If `edges` table has entries with `kind=ref` for this name → use
     them (Rust).
   - Else if `lang ∈ {ts, tsx, js, ruby}` → fall through to the
     streaming `refs::find_refs`.
   - Else → fall through to `find_callers` (which has its own ladder).
3. If the dispatch is *confusing* (multiple paths arrive at the same
   answer by accident, or a language is silently missing), simplify
   the dispatch to a single match before documenting.
4. Add a new subsection "Ref-finding routing" to
   `crates/crabcc-core/docs/HOW_IT_WORKS.md` under "Query pipeline"
   (or wherever it fits) with a table:

   | Language | Path | Source of truth |
   |---|---|---|
   | rust | `edges` table | `extract::walk_edges → ref_target` |
   | typescript / tsx / js / ruby | streaming identifier scan | `refs::find_refs` |
   | go / python / swift / bash / java | falls back to `find_callers` | `query::query_callers` |

5. If T8 has landed any of the langs in row 3 as `edges`-based, update
   the table.

### Verification

```bash
task                     # full build + test
grep -A 20 "Ref-finding routing" crates/crabcc-core/docs/HOW_IT_WORKS.md
# Subsection exists and includes the table.
```

---

## T6 — `swe-deps` — check whether `gpui-component` is now on `tree-sitter = 0.26`

**Branch from:** `main` (independent of the followups branch)
**Output PR base:** `main`
**Target files:** `Cargo.toml`, `crates/crabcc-desktop/Cargo.toml` —
*if* the bump is feasible
**Size:** small (one bump) or zero (file an issue and back out)
**Acceptance:** either a passing `cargo check --workspace` with the
desktop crate re-joined, or a written reason why we still can't.

### Problem

`Cargo.toml:12-28` documents the exclusion of `crates/crabcc-desktop`:

> dep on `tree-sitter = "0.25"` (links = "tree-sitter", so cargo
> enforces a single version workspace-wide), and crabcc-core is on
> tree-sitter 0.22 with its grammar fleet at 0.21. Joining the
> workspace would force a coordinated tree-sitter-22→25 bump across
> six grammar crates.

This comment is **stale**: workspace is now on `tree-sitter = "0.26"`
plus ten grammar crates (see the workspace `[workspace.dependencies]`
block). The actual gap is `0.25 (gpui-component) ↔ 0.26 (us)`.

### Brief

1. Check upstream `gpui-component` for the current `tree-sitter` pin.
   Source: https://github.com/longbridge/gpui-component (verify URL
   via `crates/crabcc-desktop/Cargo.toml` for the actual git source if
   different).
2. If it ships `tree-sitter = "0.26"` or later: bump the desktop
   crate's pin, re-add it to the workspace `members`, run
   `task local-ci` from the workspace root. Update the explanatory
   comment in `Cargo.toml:12-28` to match the new state.
3. If it's still on 0.25 (or another mismatched version): file a
   tracking issue in this repo with the upstream's pin, link to the
   gpui-component issue tracker if a bump is in flight there, and
   update the comment in `Cargo.toml` with the current upstream pin
   and a follow-up cadence (e.g. "next check 2026-08").
4. Do **not** force the unification by pinning down to 0.25 — the
   grammar crates we depend on (especially `tree-sitter-bash`,
   `tree-sitter-java`, `tree-sitter-swift`) only ship API-compatible
   builds against ≥0.26.

### Verification

If bumping:
```bash
cargo check --workspace
cd crates/crabcc-desktop && cargo check && cd ../..
```

If filing: link to the issue from the comment in `Cargo.toml`.

---

## T7 — `swe-refactor` — introduce `LanguageWalker` trait (defer to v3.4)

**Branch from:** after T2 lands
**Output PR base:** `main`
**Target files:** `crates/crabcc-core/src/extract/`
**Size:** medium; mostly mechanical once T2 has surfaced the shape

### Problem

After T2 the per-language modules each export the same six free
functions: `is_callable`, `call_target`, `ref_target`, `symbol_kind_for`,
`signature_for`, `visibility_for`. The `mod.rs` dispatcher has six
parallel `match lang { … }` ladders against the language-tag string.
Replace these with a single trait + registry.

### Brief

This is a **design-first** task. Before writing code:

1. Draft the trait. Likely shape:
   ```rust
   pub(super) trait LanguageWalker {
       fn ts_language() -> tree_sitter::Language;
       fn is_callable(kind: &str) -> bool;
       fn call_target(node: &tree_sitter::Node, src: &[u8]) -> Option<(String, u32)>;
       fn ref_target(node: &tree_sitter::Node, src: &[u8]) -> Option<(String, u32)>;
       fn symbol_kind_for(kind: &str) -> Option<crate::types::SymbolKind>;
       fn signature_for(node: &tree_sitter::Node, src: &[u8]) -> Option<String>;
       fn visibility_for(node: &tree_sitter::Node, src: &[u8]) -> Option<String>;
   }
   ```
   Decide whether `LanguageWalker` is associated-fn-only (current
   shape) or whether holding a `&Self` adds value. Recommend
   associated-fn-only — there's no per-language state.

2. Each lang module gets a unit struct `pub(super) struct Rust;`
   that `impl LanguageWalker for Rust`. The free fns move into the
   impl block.

3. `mod.rs` carries one `dispatch(lang: &str, op: …) -> …` that
   matches on `lang` once and calls the trait fn. Alternatively use
   the *Strategy + Registry* pattern: a `lazy_static<HashMap<&'static
   str, &'static dyn LanguageWalker>>`. Trade-off: trait-object
   dispatch costs a vtable lookup per node-kind question on the
   tight extract path; the static match is ~0 cost. Go with the
   static match unless you find a clear reason otherwise.

4. Adding a new language now means one new file + one row in the
   dispatch match. Document this in
   `crates/crabcc-core/docs/HOW_IT_WORKS.md` ("Add a new language"
   recipe).

### Verification

```bash
task              # full build + test, no perf regression
cargo bench -p crabcc-core extract  # if a bench exists
```

This task is **deferred to v3.4** because (a) T2's free-function
split already delivers the diff-locality win, and (b) the trait shape
will be clearer after T8 fans out ref-edges to more languages.

---

## T8 — `swe-feature` — add `kind=ref` extraction parity for non-Rust languages

**Branch from:** after T2 lands (one mini-PR per language is fine)
**Output PR base:** `main`
**Target files:** `crates/crabcc-core/src/extract/lang/<lang>.rs`,
new fixtures + tests
**Size:** *one task per language* — Go, Python, TypeScript/TSX,
JavaScript, Java, Swift each get a dedicated agent run.
**Acceptance:** `lookup refs <name>` returns hits for type uses in the
target language, matching the Rust behaviour shipped in 3.2.0.

### Problem

`ref_target` in `extract.rs` (post-T2: `extract/lang/rust.rs::ref_target`)
emits `kind=ref` edges for every `type_identifier` node in
non-definition position. Every other language's `ref_target` is
`None` — even when the language has an equivalent concept (Go
`type_identifier`, Python `attribute`, TS/Java `type_identifier`).

`lookup refs Foo` therefore works for Rust only and silently
returns `[]` for the others (modulo the JS/TS/Ruby streaming fallback,
which has different semantics — see T5).

### Brief (per language — dispatch one agent per row)

| Language | tree-sitter node kinds to emit `ref` for | Notes |
|---|---|---|
| go | `type_identifier`, `qualified_type` | Skip identifier nodes that are field selectors (`x.Foo` already covered by `call` edges). |
| python | `identifier` (when parent is `subscript` / `argument_list` / `assignment.right`); careful — Python has no real "type identifier" — restrict by parent rather than overshoot. | The risk is false positives; bias toward false negatives. |
| typescript | `type_identifier`, `type_annotation` children, generic type args | Same shape as Rust — should be a near-copy of `rust::ref_target`. |
| tsx | shares logic with typescript | Reuse the same extractor, switch by `language()` outcome. |
| javascript | `identifier` in `new` expressions, in `instanceof` RHS | JS has no type system to mine; restrict to construction / instanceof to avoid generating ref-edge noise for every variable use. |
| java | `type_identifier`, `generic_type` arguments | Mirror Rust closely. |
| swift | `type_identifier`, `user_type` | tree-sitter-swift node names — verify against the grammar JSON. |
| bash | **skip** — no type system, no sensible "ref" concept. | Document explicitly. |
| ruby | already covered by the streaming `refs::find_refs` fallback (T5) | Decide whether to add `edges`-based ref-extraction or stay on the streaming path. Recommend stay until usage tells us otherwise. |

For each language:

1. Add an integration fixture under
   `crates/crabcc-core/tests/fixtures/refs_<lang>/` containing a
   small file with the type/identifier under test.
2. Write a test in `extract` or `query` (probably the latter) that
   indexes the fixture, queries `find_refs("Foo")`, and asserts the
   expected hit count + lines.
3. Update `CHANGELOG.md` Unreleased section ("`lookup refs` now works
   for <lang>").
4. Once **all** non-Rust languages land, bump `ref_edges_built`
   semantics? — no. The marker key remains "any ref-edge extraction
   ran." A *separate* per-language readiness gate would be over-
   engineered; one rebuild after upgrade covers it.

### Verification (per language)

```bash
cargo test -p crabcc-core --lib '<lang>'
cargo test -p crabcc-core --tests 'refs_<lang>'
task smoke
```

End-to-end on a real repo:

```bash
crabcc index
crabcc lookup refs <typename_known_to_appear_in_lang>
# Returns hits with the right line numbers.
```

---

## Cross-task verification (post-merge of T1–T6)

```bash
git checkout main
git pull
task local-ci         # fmt + clippy + tests + doc-build
task smoke
task memory-smoke

# Stale-index repair (T4) end-to-end
sqlite3 .crabcc/index.db "DELETE FROM meta WHERE key='ref_edges_built';"
# MCP + LSP must surface this and either rebuild or refuse.
```

## Out of scope

- **Edition 2024 / clap 5 / ast-grep 0.43** — covered by the broader
  3.x backlog, not this slice.
- **Removing `bumpalo`** — the `_scratch` arena in `extract` is a
  no-op today; T2 should move it intact, T7 may delete it if the trait
  shape obviates it.
- **The `crates/crabcc-cli/src/main.rs` size** (still ~2250 LoC after
  the 3.2.0 alias removal) — natural follow-up but unrelated to the
  tree-sitter axis.

## Notes for the dispatcher

- Every task above is written so an `swe-*` agent can pick it up
  cold; the agent does not need this whole document, just its
  section.
- T1 + T4 + T6 are independent and safe to run in parallel as the
  first wave.
- T2 is the gating refactor for T3 / T7 / T8 — land it before
  dispatching the dependents.
- `task local-ci` must pass on every PR. Per the CI changes in
  `31baa19`, this now includes the `wild` linker path on Linux —
  agents on macOS hosts can skip the wild check locally; CI will catch
  any regression.
