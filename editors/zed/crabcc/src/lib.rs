//! Zed extension that registers `ucracc-lsp` — crabcc's navigation +
//! retrieval language server — as an additional server for Rust, the
//! TS/JS family, Python, Ruby, Go, Swift, Java, YAML, and Markdown.
//!
//! Zed can't bind an arbitrary LSP binary to a language through
//! `settings.json` alone (unlike Neovim's `lspconfig`); a server that
//! isn't built in has to be contributed by an extension. That's all this
//! crate is: a thin shim that tells Zed *how to launch* `ucracc-lsp` and
//! *how to forward* the user's `lsp.ucracc-lsp.*` settings to it.
//!
//! Binary resolution order (first hit wins):
//!   1. `lsp.ucracc-lsp.binary.path` from Zed settings (explicit override).
//!   2. `ucracc-lsp` on the worktree `$PATH` (the normal case — install it
//!      with `cargo install --path crates/ucracc-lsp` or `crabcc`'s
//!      release tarball).
//!
//! The server self-discovers `<root>/.crabcc/index.db`; point it elsewhere
//! with `lsp.ucracc-lsp.initialization_options.indexPath` (handy when this
//! workspace's index lives outside the checkout — an out-of-tree build or a
//! remote host. It's a location override only; the index must still have
//! been built for this same workspace root).

use zed_extension_api::{
    self as zed,
    settings::LspSettings,
    Command, LanguageServerId, Result, Worktree,
};

const SERVER_BINARY: &str = "ucracc-lsp";

struct CrabccExtension;

impl CrabccExtension {
    /// Resolve the `ucracc-lsp` binary, honoring an explicit
    /// `lsp.ucracc-lsp.binary.path` override before falling back to `$PATH`.
    ///
    /// Resolution is **per-worktree** and deliberately uncached:
    /// `worktree.which` is evaluated against *that worktree's* host `$PATH`,
    /// so a local project and a remote SSH project in the same Zed session
    /// each get the binary that actually exists on their respective host.
    /// Caching the first hit across worktrees would launch a local path on a
    /// remote host (or vice-versa) and break the documented remote workflow.
    fn binary_path(worktree: &Worktree) -> Result<String> {
        // Explicit override from settings always wins.
        if let Some(path) = LspSettings::for_worktree(SERVER_BINARY, worktree)
            .ok()
            .and_then(|s| s.binary)
            .and_then(|b| b.path)
        {
            return Ok(path);
        }

        // Otherwise discover on this worktree's `$PATH`.
        worktree.which(SERVER_BINARY).ok_or_else(|| {
            format!(
                "`{SERVER_BINARY}` was not found on $PATH. Install it with \
                 `cargo install --path crates/ucracc-lsp` (or from a crabcc \
                 release), then run `crabcc index` in the project root. To use \
                 a specific binary, set `lsp.{SERVER_BINARY}.binary.path` in \
                 your Zed settings."
            )
        })
    }
}

impl zed::Extension for CrabccExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = Self::binary_path(worktree)?;

        let settings = LspSettings::for_worktree(SERVER_BINARY, worktree).ok();
        let binary = settings.and_then(|s| s.binary);

        // ucracc-lsp speaks LSP over stdio and takes no args of its own;
        // anything in `binary.arguments` is passed straight through.
        let args = binary
            .as_ref()
            .and_then(|b| b.arguments.clone())
            .unwrap_or_default();

        // Start from the worktree shell env (so the server inherits e.g.
        // CRABCC_HOME / proxy vars) then layer any explicit overrides.
        let mut env = worktree.shell_env();
        if let Some(extra) = binary.and_then(|b| b.env) {
            env.extend(extra);
        }

        Ok(Command {
            command,
            args,
            env,
        })
    }

    /// Forward `lsp.ucracc-lsp.initialization_options` (e.g. `indexPath`)
    /// to the server's `initialize` request.
    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER_BINARY, worktree)
            .ok()
            .and_then(|s| s.initialization_options))
    }

    /// Forward `lsp.ucracc-lsp.settings` as the workspace configuration.
    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER_BINARY, worktree)
            .ok()
            .and_then(|s| s.settings))
    }
}

zed::register_extension!(CrabccExtension);
