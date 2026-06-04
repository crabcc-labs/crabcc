# crabcc — Zed extension

Registers **ucracc-lsp** (crabcc's navigation + retrieval language server) as an
*additional* language server in [Zed](https://zed.dev/), so you get crabcc's
index-backed symbol navigation, references, document/workspace symbols, and call
hierarchy on top of the normal per-language tooling (rust-analyzer, pyright, …).

## Prerequisites

1. **ucracc-lsp on PATH.** crabcc does not yet publish a standalone `ucracc-lsp`
   release binary, so build/install it from the crabcc repo:
   ```bash
   cargo install --path crates/ucracc-lsp     # or: cargo build -p ucracc-lsp --release
   # ensure the resulting `ucracc-lsp` is on your PATH
   ```
2. **Index the repo** once so the server has data: `crabcc index` at the repo root.

## Install the extension

Zed → command palette → **zed: install dev extension** → choose this directory
(`editors/zed/crabcc`). Zed builds it to `wasm32-wasip1` and loads it.

## Languages

Attaches to: Rust, TypeScript, TSX, JavaScript, Python, Ruby, Go, YAML, Markdown
(see `extension.toml`). Swift, Java, and Shell Script also work — add them to the
`languages` list if those Zed language extensions are installed and the names
match Zed exactly.

## Build / verify

```bash
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
```

> [!IMPORTANT]
> This extension was scaffolded but **not compile-verified** in CI yet. Two things
> to confirm:
> - The `zed_extension_api` version in `Cargo.toml` must match your target Zed
>   release. If `cargo build --target wasm32-wasip1` fails on an API mismatch,
>   bump it to the version used by current entries in `zed-industries/extensions`.
> - `Worktree::which` returns `Option<String>` in the API used here; if your
>   pinned `zed_extension_api` differs, adjust the resolution in `src/lib.rs`.

## How it fits

Complements the other crabcc↔fleet integrations: the agent-runner bakes crabcc's
MCP for in-container agents (crabcc-labs/crabcc#621) and `crabcc-agent` exposes it
over A2A (peterlodri-sec/agentfield#170); this brings the same index to a human's
editor.
