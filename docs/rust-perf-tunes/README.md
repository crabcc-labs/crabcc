# Rust Performance Tunes — machine-readable optimization dataset

A workflow for scaling crabcc's performance wisdom ("tunes") into an extensive,
machine-readable dataset that coding agents (Claude, GPT-4o, custom
LangChain/LlamaIndex pipelines) can parse, understand, and apply to raw code.

Coding agents fail on raw markdown tips; they succeed when given explicit
**AST matching rules**, **before/after transformations**, and **contextual
metadata**. The pieces here schemify that wisdom so it can be injected directly
into agent prompts or a RAG vector DB.

Files in this directory:

- [`tunes_schema.json`](./tunes_schema.json) — JSON Schema (draft-07) modeling a
  tune as a deterministic transformation rule.
- [`tunes.example.json`](./tunes.example.json) — one valid example tune
  (`RUST-OPT-001`) that validates against the schema and seeds the scanner.
- [`rust_opt_agent.py`](./rust_opt_agent.py) — the two-pass scanner + refactor
  agent loop.

---

## Phase 1: The schema (`tunes_schema.json`)

Model a "tune" as a deterministic transformation rule rather than just a tip:
metadata, the anti-pattern regex/AST rules, the target fix, and authoritative
verification links. See [`tunes_schema.json`](./tunes_schema.json) for the full
draft-07 definition. A tune requires: `id` (`^RUST-OPT-\d{3}$`), `title`,
`impact_level` (HIGH/MEDIUM/LOW), `category`
(Memory/Async/Collections/I/O/Compiler), `description`, a `trigger_condition`
(`ast_node_types`, `anti_pattern_regex`, `explanation`), a `remediation`
(`before_code`, `after_code`, `dependencies`), and up to 3 `references`.

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

1. **Pass 1 (Scanner):** a light script/agent uses the `trigger_condition`
   fields to flag which files might violate specific rules.
2. **Pass 2 (Refactorer):** the target file and *only the matching JSON rules*
   are fed into the coding agent's execution context.

Reference implementation: [`rust_opt_agent.py`](./rust_opt_agent.py)
(`RustOptAgent.scan_and_identify_tunes` → `optimize_file`).

## Phase 4: Agent execution blueprint (CI/CD integration)

```
[Raw Code Commit]
       │
       ▼
[AST / Regex Rule Scan via Python] ──(No Matches)──► [Exit Safe]
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
