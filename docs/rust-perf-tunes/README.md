# Rust Performance Tunes — machine-readable optimization dataset

A workflow for scaling crabcc's performance wisdom ("tunes") into an extensive,
machine-readable dataset that coding agents (Claude, GPT-4o, custom
LangChain/LlamaIndex pipelines) can parse, understand, and apply to raw code.

Coding agents fail on raw markdown tips; they succeed when given explicit
**AST matching rules**, **before/after transformations**, and **contextual
metadata**. The pieces here schemify that wisdom so it can be injected directly
into agent prompts or a RAG vector DB.

The trigger matching uses **real AST** (tree-sitter via [`ast-grep`](https://ast-grep.github.io/)),
not regex — so a pattern matches the actual syntax node and ignores occurrences
in comments and string literals. The scanner is written in
[Amber](https://amber-lang.com/) (typed bash) and compiles to portable POSIX
bash, orchestrating modern CLI tools (`ast-grep` + `jq`).

Files in this directory:

- [`tunes_schema.json`](./tunes_schema.json) — JSON Schema (draft-07) modeling a
  tune as a deterministic transformation rule.
- [`tunes.example.json`](./tunes.example.json) — one valid example tune
  (`RUST-OPT-001`) that validates against the schema and seeds the scanner.
- [`scan.ab`](./scan.ab) — the Pass-1 scanner (Amber source). Build with
  `amber build scan.ab scan.sh`.
- [`scan.sh`](./scan.sh) — the compiled scanner (committed so it runs **without**
  Amber installed — only `ast-grep` + `jq` are needed).
- [`tune_rows.jq`](./tune_rows.jq), [`refactor_context.jq`](./refactor_context.jq)
  — the jq programs the scanner drives (kept as files to avoid Amber
  brace-interpolation).
- [`fixtures/sample.rs`](./fixtures/sample.rs) — a Rust fixture demonstrating the
  AST match (the real `Vec::new()` is flagged; the ones in a comment and a string
  are not).

---

## Phase 1: The schema (`tunes_schema.json`)

Model a "tune" as a deterministic transformation rule rather than just a tip:
metadata, the anti-pattern regex/AST rules, the target fix, and authoritative
verification links. See [`tunes_schema.json`](./tunes_schema.json) for the full
draft-07 definition. A tune requires: `id` (`^RUST-OPT-\d{3}$`), `title`,
`impact_level` (HIGH/MEDIUM/LOW), `category`
(Memory/Async/Collections/I/O/Compiler), `description`, a `trigger_condition`
(`ast_node_types`, `ast_grep_pattern`, `anti_pattern_regex`, `explanation`), a
`remediation` (`before_code`, `after_code`, `dependencies`), and up to 3
`references`. The scanner prefers `ast_grep_pattern` (proper AST);
`anti_pattern_regex` is a documented fallback for tunes that don't yet have one.

## Phase 2: Generating an extensive set (the engine)

Use a frontier LLM as a structured data generator to scale this database to
100+ items. Feed it `tunes_schema.json` alongside this strict prompt:

> **System Prompt for Generator LLM:**
> You are a senior Rust systems engineer and static analysis compiler expert.
> Your job is to output a valid JSON array matching the provided
> `RustPerformanceTune` schema.
> Discover deeply technical optimization tricks from sources like the Rust
> Performance Book, Tokio source code, Cargo profiles, and `std::mem`/`std::sync`
> deep dives. Avoid basic advice like "don't use global variables". Focus on
> memory layout, thread-contention reduction, zero-copy, cache-line
> optimization, and asynchronous loop safety.
> Ensure the `trigger_condition` precisely outlines how a static analysis
> linting engine or a regex pattern would catch the anti-pattern in raw source
> code.

## Phase 3: Utilizing the schema with coding agents

Do not dump 100 JSON items into the context window (attention dilution). Use a
**two-pass tooling pipeline**:

1. **Pass 1 (Scanner):** `scan.sh` runs each tune's `ast_grep_pattern` through
   ast-grep (tree-sitter AST) over the target, and writes `refactor-context.json`
   — only the matched tunes (full rules) plus the target file.
2. **Pass 2 (Refactorer):** feed `refactor-context.json` to the coding agent, so
   its context holds *only* the matching rules — no attention dilution.

Run it:

```bash
# build once (needs amber); or just use the committed scan.sh
amber build scan.ab scan.sh

# scan a file or directory
./scan.sh tunes.example.json fixtures/sample.rs
#   ⚠ RUST-OPT-001: 1 site(s) — pattern: Vec::new()
#   🤖 wrote refactor-context.json for the refactor agent
```

The regex in the fixture comment/string is ignored — only the real AST node
matches. Regex-only tunes (no `ast_grep_pattern`) are skipped by the current
scanner; an `rg` fallback for those is the obvious next step.

## Phase 4: Agent execution blueprint (CI/CD integration)

```
[Raw Code Commit]
       │
       ▼
[AST Scan via ast-grep (scan.sh)] ──(No Matches)──► [Exit Safe]
       │
       ├─► (Matches Found: E.g., RUST-OPT-001 & RUST-OPT-030)
       ▼
[Isolate Rules & Code Slice]
       │
       ▼
[Agent Refactoring Execution Loop]
       │
       ▼
[Run `cargo check` and `cargo test`]
       │
       ├──► (Tests Fail) ──► [Feedback Errors to Agent for Self-Correction Iteration]
       └──► (Tests Pass) ─► [Auto-generate Pull Request with Optimization References]
```
