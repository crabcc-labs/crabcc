# crabcc for Zed

A [Zed](https://zed.dev) extension that wires **`ucracc-lsp`** — crabcc's
navigation + retrieval language server — into Zed as an *additional* server
for Rust, TypeScript/TSX, JavaScript, Python, Ruby, Go, Swift, Java, YAML,
and Markdown.

It runs **alongside** the semantic server for each language
(rust-analyzer, pyright, gopls, …). Zed merges results from every server
bound to a buffer, so you keep your type-aware diagnostics/completion and
gain, on top:

- **Repo-wide `workspace/symbol`** backed by crabcc's tantivy index
  (sub-microsecond when warm).
- **Call hierarchy** (incoming/outgoing) from crabcc's edge table.
- **Instant cold start** — `ucracc-lsp` lazy-opens its index, so it never
  blocks Zed's startup.
- **AI-first execute-commands** (`ucracc.memory.search`, `ucracc.webfetch`,
  `ucracc.rerank`) for agents driving Zed.

> Why an extension and not just `settings.json`? Zed only lets
> `settings.json` *configure* servers it already knows about; binding a new
> LSP binary to a language requires an extension. This crate is that shim —
> it tells Zed how to launch `ucracc-lsp` and forwards your
> `lsp.ucracc-lsp.*` settings to it.

## Prerequisites

1. **Install the server binary** so it's on your `$PATH`:

   ```bash
   cargo install --path crates/ucracc-lsp   # from a crabcc checkout
   # or grab `ucracc-lsp` from a crabcc release tarball
   ```

2. **Build the index** once per project (and let crabcc keep it fresh):

   ```bash
   cd /path/to/your/project
   crabcc index
   ```

## Quick install (script)

From a crabcc checkout, one command does the automatable parts — installs
the `ucracc-lsp` binary, builds your project index, and build-checks the
extension:

```bash
bash install/zed.sh                       # nav-only build
bash install/zed.sh --features memory,fetch,rerank   # + AI execute-commands
```

It finishes by printing the single Zed action to register the extension
(or pass `--headless` to attempt a fully UI-free drop-in — experimental;
see `install/zed.sh --help`). Manual steps below.

## Install the extension (dev)

Until this ships to the Zed extension registry, install it as a dev
extension:

1. Open Zed → command palette → **`zed: install dev extension`**.
2. Pick this directory (`editors/zed/crabcc`).

Zed builds the WASM component (needs the `wasm32-wasip1` Rust target, which
`rustup target add wasm32-wasip1` provides) and loads `ucracc-lsp` for the
languages above. Open a file in an indexed project and try
**`editor: go to definition`** or **`project: open symbol`** (workspace
symbols).

## Configure (`settings.json`)

All keys are optional. Defaults: find `ucracc-lsp` on `$PATH`, use
`<project>/.crabcc/index.db`.

```jsonc
{
  "lsp": {
    "ucracc-lsp": {
      // Pin a specific binary (skip $PATH discovery).
      "binary": {
        "path": "/Users/you/.cargo/bin/ucracc-lsp",
        "arguments": [],
        "env": { "UCRACC_LOG": "warn" }
      },
      // Forwarded to the server's `initialize`.
      "initialization_options": {
        // Point at a .crabcc dir that isn't <project>/.crabcc — useful for
        // monorepos or when the index is built out-of-tree.
        "indexPath": ".crabcc"
      }
    }
  }
}
```

### Don't want it on a given language?

Zed lets you disable a server per-language without uninstalling:

```jsonc
{
  "languages": {
    "Markdown": { "language_servers": ["!ucracc-lsp"] }
  }
}
```

## Remote development (Zed SSH)

Zed runs language servers on the **remote** host, not your Mac. So when you
open a project over SSH (e.g. a `hester`-style dev box):

- Install `ucracc-lsp` **on the remote host** (`cargo install` there, or
  ship the release binary) so it lands on the remote `$PATH`.
- Run `crabcc index` **on the remote host**, against the remote checkout.
- The extension itself only needs to be installed once in your local Zed;
  Zed handles launching the server on the remote side.

If the index lives somewhere non-standard on the remote, set
`initialization_options.indexPath` (relative paths resolve against the
remote worktree root).

## Troubleshooting

- **"`ucracc-lsp` was not found on $PATH"** — install the binary (above) or
  set `lsp.ucracc-lsp.binary.path`. Check the resolved env with Zed's
  **`zed: open log`**.
- **No workspace symbols / empty go-to-definition** — the index is missing
  or stale. Run `crabcc index` (it builds both `index.db` and the
  `tantivy/` sidecar that `workspace/symbol` needs).
- **Server logs** — set `"env": { "UCRACC_LOG": "debug" }` under
  `binary`; output shows up in Zed's LSP logs (`dev: open language server
  logs`).

See [`crates/ucracc-lsp/docs/ZED.md`](../../../crates/ucracc-lsp/docs/ZED.md)
for the deeper guide.
