# crabcc for Zed

A [Zed](https://zed.dev) extension that wires **`ucracc-lsp`** — crabcc's
navigation + retrieval language server — into Zed as an *additional* server
for Rust, TypeScript/TSX, JavaScript, Python, Ruby, Go, Swift, Java, Shell,
YAML, and Markdown.

It runs **alongside** the semantic server for each language
(rust-analyzer, pyright, gopls, …). Zed merges results from every server
bound to a buffer, so you keep your type-aware diagnostics/completion and
gain, on top:

- **Repo-wide `workspace/symbol`** backed by crabcc's native symbol search
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

## About this repository

This repo is the **published, registry-ready mirror** of the extension. The
source of truth lives in the crabcc repository at
[`editors/zed/crabcc`](https://github.com/crabcc-labs/crabcc/tree/main/editors/zed/crabcc);
this copy is kept in sync so the [Zed extension registry](https://github.com/zed-industries/extensions)
can build from a public location (crabcc itself is a private monorepo).
Licensed **GPLv3** — see [License](#license).

## Prerequisites

### 1. Install the `ucracc-lsp` server binary

The extension launches `ucracc-lsp`; it must be resolvable on the host where
Zed runs the language server. In resolution order, the extension uses:

1. an explicit `lsp.ucracc-lsp.binary.path` in your Zed settings, then
2. `ucracc-lsp` on the worktree `$PATH`, then
3. a prebuilt binary auto-downloaded from this repo's GitHub releases
   (`crabcc-labs/zed-crabcc`) — zero-setup, once release assets are
   published here.

Until prebuilt assets are published, put the binary on your `$PATH`:

```bash
cargo install ucracc-lsp                       # from crates.io, if published
# or, from a crabcc checkout:
cargo install --path crates/ucracc-lsp
# or download `ucracc-lsp` from a crabcc release:
#   https://github.com/crabcc-labs/crabcc/releases
```

### 2. Build the project index

Once per project (and let crabcc keep it fresh):

```bash
cd /path/to/your/project
crabcc index            # builds .crabcc/index.db
```

## Install the extension

### From the Zed extension registry

Once published: open Zed → **Extensions** (`zed: extensions`) → search
**crabcc** → Install. Zed downloads and builds the extension for you.

### As a dev extension (before/without the registry)

1. Clone this repository (or use `editors/zed/crabcc` from a crabcc checkout).
2. Open Zed → command palette → **`zed: install dev extension`**.
3. Pick the extension directory — the **root of this repo** if you cloned
   `zed-crabcc`, or `editors/zed/crabcc` inside a crabcc checkout.

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
        // Point at a .crabcc dir that isn't <project>/.crabcc — e.g. an
        // out-of-tree/shared-cache index built FOR THIS workspace root.
        // (Location override only; not a different root's index.)
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

Zed runs language servers on the **remote** host, not your local machine. So
when you open a project over SSH:

- Install `ucracc-lsp` **on the remote host** (`cargo install` there, or ship
  the release binary) so it lands on the remote `$PATH`.
- Run `crabcc index` **on the remote host**, against the remote checkout.
- The extension itself only needs to be installed once in your local Zed; Zed
  handles launching the server on the remote side.

If the index lives somewhere non-standard on the remote, set
`initialization_options.indexPath` (relative paths resolve against the remote
worktree root).

## Troubleshooting

- **"`ucracc-lsp` was not found on $PATH"** — install the binary (above) or
  set `lsp.ucracc-lsp.binary.path`. Check the resolved env with Zed's
  **`zed: open log`**.
- **No workspace symbols / empty go-to-definition** — the index is missing or
  stale. Run `crabcc index` (it builds `index.db`, which `workspace/symbol`
  reads directly).
- **Server logs** — set `"env": { "UCRACC_LOG": "debug" }` under `binary`;
  output shows up in Zed's LSP logs (`dev: open language server logs`).

## License

GPLv3 — see [`LICENSE`](./LICENSE) and [`NOTICE.md`](./NOTICE.md) for what
that means for forks and commercial use (copyleft + attribution).
© crabcc-labs.

For the deeper server-side guide, see
[`crates/ucracc-lsp/docs/ZED.md`](https://github.com/crabcc-labs/crabcc/blob/main/crates/ucracc-lsp/docs/ZED.md)
in the crabcc repository.
