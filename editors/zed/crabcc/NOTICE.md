# Licensing & attribution

The **crabcc for Zed** extension (`zed_crabcc`) is licensed under the
**GNU General Public License v3.0** — see [`LICENSE`](./LICENSE).

Copyright © crabcc-labs. Source of truth:
<https://github.com/crabcc-labs/crabcc> (extension at `editors/zed/crabcc`);
published, in-sync copy for the Zed registry at
<https://github.com/crabcc-labs/zed-crabcc>.

## What that means if you fork, modify, or ship it

GPLv3 is **copyleft**. You're free to use, study, modify, and redistribute
this extension — including as part of a commercial offering — **but** any
version you distribute (a fork, a re-skin, a bundled product) must:

- stay licensed under **GPLv3**, and
- **make the complete corresponding source available** to recipients, and
- **retain the copyright notice and attribution** to crabcc-labs, with a
  link back to the original repository, and
- **state the changes** you made.

In short: you cannot take this extension closed-source. If you build on it,
credit the original (crabcc-labs + the repo URL) and keep your derivative
open under the same license.

## Scope — what the license does and doesn't cover

This license covers **only this extension shim** (the WASM crate that tells
Zed how to launch `ucracc-lsp`). It does **not** relicense:

- **your project's code** — the extension just runs a language server over
  stdio; your source is unaffected;
- **the `ucracc-lsp` server binary** — that's a separate program from the
  (private) crabcc monorepo, invoked as a subprocess. The extension merely
  *executes* it; there is no linking, so it's an aggregation, not a
  derivative work of the server.

If you need terms other than GPLv3 for a specific use, contact crabcc-labs.
This file is an explanatory summary, not a substitute for the full
[`LICENSE`](./LICENSE) text, which governs.
