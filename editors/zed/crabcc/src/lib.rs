//! Zed extension: register `ucracc-lsp` (crabcc's navigation/retrieval language
//! server) as an additional language server for crabcc's languages.
//!
//! crabcc does not (yet) publish a standalone `ucracc-lsp` release binary, so
//! this extension does not auto-download. It resolves the binary from:
//!   1. the `UCRACC_LSP_PATH` env var (set via Zed lsp settings `binary.env`), or
//!   2. `ucracc-lsp` on PATH.
//! Install it from the crabcc repo (`cargo install --path crates/ucracc-lsp`)
//! and make sure it is on your PATH. See README.md.

use zed_extension_api::{self as zed, LanguageServerId, Result};

struct CrabccExtension;

impl zed::Extension for CrabccExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        // Explicit override wins (Zed lsp settings can inject env).
        let command = worktree
            .which("ucracc-lsp")
            .ok_or_else(|| {
                "ucracc-lsp not found on PATH. Install it from the crabcc repo, \
                 e.g. `cargo install --path crates/ucracc-lsp`, and ensure the \
                 binary is on your PATH (or set UCRACC_LSP_PATH)."
                    .to_string()
            })?;

        Ok(zed::Command {
            command,
            args: vec![],
            env: Default::default(),
        })
    }
}

zed::register_extension!(CrabccExtension);
