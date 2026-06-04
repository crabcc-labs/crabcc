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
//! with `lsp.ucracc-lsp.initialization_options.indexPath` (handy on remote
//! hosts or monorepos where the index lives outside the worktree root).

use zed_extension_api::{
    self as zed,
    settings::LspSettings,
    Command, LanguageServerId, Result, Worktree,
};

const SERVER_BINARY: &str = "ucracc-lsp";

struct CrabccExtension {
    /// Resolved binary path, cached so we don't re-walk `$PATH` on every
    /// server (re)start within a session.
    cached_binary_path: Option<String>,
}

impl CrabccExtension {
    /// Resolve the `ucracc-lsp` binary, honoring an explicit
    /// `lsp.ucracc-lsp.binary.path` override before falling back to `$PATH`.
    fn binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<String> {
        // 1. Explicit override from settings always wins, and is never
        //    cached — the user may edit it live.
        if let Some(path) = LspSettings::for_worktree(SERVER_BINARY, worktree)
            .ok()
            .and_then(|s| s.binary)
            .and_then(|b| b.path)
        {
            return Ok(path);
        }

        // 2. Cached resolution from a previous launch this session.
        if let Some(path) = &self.cached_binary_path {
            return Ok(path.clone());
        }

        // 3. Discover on the worktree `$PATH`.
        let _ = language_server_id;
        let path = worktree.which(SERVER_BINARY).ok_or_else(|| {
            format!(
                "`{SERVER_BINARY}` was not found on $PATH. Install it with \
                 `cargo install --path crates/ucracc-lsp` (or from a crabcc \
                 release), then run `crabcc index` in the project root. To use \
                 a specific binary, set `lsp.{SERVER_BINARY}.binary.path` in \
                 your Zed settings."
            )
        })?;
        self.cached_binary_path = Some(path.clone());
        Ok(path)
    }
}

impl zed::Extension for CrabccExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = self.binary_path(language_server_id, worktree)?;

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
