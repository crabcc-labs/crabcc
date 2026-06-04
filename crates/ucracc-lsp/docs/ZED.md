# Using `ucracc-lsp` in Zed

> The short version lives in [`editors/zed/crabcc/README.md`](../../../editors/zed/crabcc/README.md).
> This is the deeper guide: how the integration works, the full settings
> surface, remote/SSH setups, the AI-first execute-command surface, and
> what to expect performance-wise.

`ucracc-lsp` is a **navigation + retrieval** server. It does not do
diagnostics, completion, formatting, or rename — those stay with the
semantic server for each language. In Zed it runs *alongside*
rust-analyzer / pyright / gopls / etc., and Zed merges results per buffer.
What it adds: repo-wide `workspace/symbol`, call hierarchy, and instant
go-to-definition/hover off a precomputed index.

---

## Why an extension is required

Zed's `settings.json` can *configure* a language server it already knows
about (override its binary, pass init options), but it can't *bind a new
LSP binary to a language* — that's a capability only extensions have.
Neovim's `lspconfig` lets you register an arbitrary `cmd`; Zed does not.
So the [`editors/zed/crabcc`](../../../editors/zed/crabcc) extension is the supported
path. It's a ~120-line WASM shim that:

1. Declares which Zed languages `ucracc-lsp` attaches to
   (`extension.toml` → `[language_servers.ucracc-lsp]`).
2. Maps Zed's language display names to the LSP `languageId`s the server
   advertises (`language_ids`).
3. Tells Zed how to launch the binary (`language_server_command`).
4. Forwards your `lsp.ucracc-lsp.initialization_options` and
   `lsp.ucracc-lsp.settings` to the server.

The language `languageId` mapping mirrors `SUPPORTED_LANGUAGE_IDS` in
[`src/lang.rs`](../src/lang.rs); keep the two in sync if you add a language.

---

## Setup

> **Shortcut:** from a crabcc checkout, `bash install/zed.sh` automates
> steps 1–2 below (binary install + index + extension build-check) and
> prints the one Zed action for step 3. `--headless` attempts a fully
> UI-free install (experimental). `install/zed.sh --help` for flags.

### 1. Install the binary

`ucracc-lsp` must be on the `$PATH` Zed sees (the worktree shell env):

```bash
cargo install --path crates/ucracc-lsp     # from a crabcc checkout
# or copy `ucracc-lsp` from a crabcc release onto $PATH
```

Features are compile-time. The default build gives you the full nav
surface (Rust, TS/JS, Python, Ruby, Go, Swift, Java, YAML, Markdown). To
get the AI-first execute-commands, build with the features you want:

```bash
cargo install --path crates/ucracc-lsp --features memory,fetch,rerank
```

| Feature  | Adds                                                            |
|----------|-----------------------------------------------------------------|
| `memory` | `ucracc.memory.search` (BM25 ⊕ vector hybrid over the repo drawer) |
| `fetch`  | `ucracc.webfetch` (cleaned main content for a URL)              |
| `rerank` | `ucracc.rerank` + auto-rerank inside `memory.search` (bge-reranker-v2-m3) |

### 2. Build the index

`ucracc-lsp` reads a precomputed index; it doesn't crawl the repo on
startup. Build it once, then let crabcc keep it fresh:

```bash
cd /path/to/project
crabcc index            # builds .crabcc/index.db
```

Without an index the server still starts but answers "empty" — Zed shows a
`window/showMessage` warning telling you to run `crabcc index`.

### 3. Install the extension

Command palette → **`zed: install dev extension`** → pick `editors/zed/crabcc`.
Zed compiles the WASM component (`rustup target add wasm32-wasip1` if you
haven't) and binds `ucracc-lsp` to the languages above.

---

## Settings reference

Everything lives under `lsp.ucracc-lsp` in Zed's `settings.json`. All
optional.

```jsonc
{
  "lsp": {
    "ucracc-lsp": {
      "binary": {
        "path": "/abs/path/to/ucracc-lsp",  // skip $PATH discovery
        "arguments": [],                      // passed through verbatim
        "env": { "UCRACC_LOG": "info" }       // merged over the shell env
      },
      "initialization_options": {
        "indexPath": ".crabcc"                // dir holding index.db
      }
    }
  }
}
```

### `initialization_options.indexPath`

Points the server at the `.crabcc` directory (the one containing
`index.db`). Relative paths resolve against the worktree
root; absolute paths are used as-is. Both `indexPath` and `index_path`
spellings are accepted. Omit it and the server uses `<root>/.crabcc`.

Use it when **this workspace's** index lives somewhere other than
`<root>/.crabcc`:
- the index is built out-of-tree (CI artifact, shared/read-only cache dir), or
- on a remote host where the `.crabcc` dir sits outside the checkout.

> `indexPath` only moves *where* the index is read from — it must still have
> been built **for this workspace root** (file paths are stored relative to
> it). Pointing at a *different* root's index — e.g. a subcrate reading the
> parent monorepo's `.crabcc` — misaligns those paths and isn't supported;
> build a per-root index instead.

### `binary.env.UCRACC_LOG`

Tracing filter for the server (default `info`). Set `debug` while
diagnosing; logs surface in Zed's **`dev: open language server logs`**.

### Disabling per language

```jsonc
{ "languages": { "YAML": { "language_servers": ["!ucracc-lsp"] } } }
```

---

## Remote development (Zed over SSH)

Zed's remote model runs language servers on the **remote** host and only
the UI on your Mac. Concretely, for an SSH project on a dev box:

1. The **extension** is installed once in your local Zed — it travels with
   the connection; you don't install it remotely.
2. The **binary** (`ucracc-lsp`) must be installed on the **remote** host's
   `$PATH`. `cargo install --path crates/ucracc-lsp` on the box, or scp a
   release binary into `~/.local/bin`.
3. The **index** must be built on the **remote** host:
   `crabcc index` against the remote checkout.
4. If the remote index isn't at `<root>/.crabcc`, set
   `initialization_options.indexPath` (resolved against the remote
   worktree root).

This is the natural fit for "connect my agents/environments into Zed":
each remote box carries its own binary + index, the Mac just drives the
UI.

---

## AI-first: the execute-command surface

Beyond navigation, `ucracc-lsp` exposes `workspace/executeCommand`
endpoints aimed at agents and retrieval-augmented workflows. They're
feature-gated (see the install step). Wire shape (LSP JSON-RPC):

```json
{ "method": "workspace/executeCommand",
  "params": { "command": "ucracc.memory.search", "arguments": ["concurrency model", 5] } }
```

| Command               | Args                                  | Returns |
|-----------------------|----------------------------------------|---------|
| `ucracc.memory.search`| `[query, limit?]`                      | hybrid (BM25 ⊕ vector) hits from the repo memory drawer; reranked if `rerank` is on |
| `ucracc.webfetch`     | `[url]`                                | cleaned main content + title |
| `ucracc.rerank`       | `[query, docs[], top_n?]`              | cross-encoder-scored `{index, score, document}` |

Commands compiled out still answer (with `{"error": "...built without X feature"}`)
so a client can feature-detect without crashing.

Agents driving Zed (via the ACP/agent surface or a custom client) can call
these directly to pull repo memory or fetch + rerank context inline — the
same retrieval surface the crabcc CLI/MCP server expose, now reachable from
inside the editor session.

---

## What to expect (performance)

Measured on M-series Apple silicon (see [`../README.md`](../README.md#performance)):

| Operation           | Cold     | Warm (cached) |
|---------------------|----------|---------------|
| `initialize` (lazy) | ~24 µs   | —             |
| `documentSymbol`    | 8.7 µs   | 3.7 µs        |
| `definition`        | 7.2 µs   | 1.1 µs        |
| `hover`             | 6.9 µs   | 1.2 µs        |
| `workspace/symbol`  | 604 µs   | 1.1 µs        |

The index opens lazily on the first request after launch, so Zed startup
never blocks on it. Keystroke reparse on a ~3 KLOC file is ~162 µs
(incremental).

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| `ucracc-lsp was not found on $PATH` | Install the binary, or set `lsp.ucracc-lsp.binary.path`. Verify with `dev: open language server logs`. |
| Empty workspace symbols / go-to-definition | Index missing or stale — run `crabcc index` (`workspace/symbol` reads it directly). |
| Stale results after big out-of-editor changes | `crabcc refresh` (incremental) or `crabcc index` (full) — fuzzy/prefix read the live index. |
| Want quiet logs | `binary.env.UCRACC_LOG = "warn"`. |
| Extension won't build | `rustup target add wasm32-wasip1`. |
