//! Zed extension that registers `ucracc-lsp` — crabcc's navigation +
//! retrieval language server — as an additional server for Rust, the
//! TS/JS family, Python, Ruby, Go, Swift, Java, Shell, YAML, and Markdown.
//!
//! Zed can't bind an arbitrary LSP binary to a language through
//! `settings.json` alone (unlike Neovim's `lspconfig`); a server that
//! isn't built in has to be contributed by an extension. That's all this
//! crate is: a thin shim that tells Zed *how to launch* `ucracc-lsp` and
//! *how to forward* the user's `lsp.ucracc-lsp.*` settings to it.
//!
//! Binary resolution order (first hit wins):
//!   1. `lsp.ucracc-lsp.binary.path` from Zed settings (explicit override).
//!   2. `ucracc-lsp` on the worktree `$PATH` (the normal local/remote case —
//!      install it with `cargo install ucracc-lsp` or a crabcc release).
//!   3. A prebuilt release downloaded from [`RELEASE_REPO`] and cached under
//!      the extension's work dir (zero-setup path for registry users).
//!
//! The server self-discovers `<root>/.crabcc/index.db`; point it elsewhere
//! with `lsp.ucracc-lsp.initialization_options.indexPath` (handy when this
//! workspace's index lives outside the checkout — an out-of-tree build or a
//! remote host. It's a location override only; the index must still have
//! been built for this same workspace root).

use zed_extension_api::{
    self as zed, settings::LspSettings, Architecture, Command, DownloadedFileType,
    GithubReleaseOptions, LanguageServerId, LanguageServerInstallationStatus, Os, Result, Worktree,
};

const SERVER_BINARY: &str = "ucracc-lsp";

/// Public companion repo that publishes prebuilt `ucracc-lsp` binaries as
/// GitHub release assets. crabcc itself is a private monorepo, so the
/// public release artifacts (and this extension's published source) live
/// here. Tier-3 auto-download is inert until this repo publishes assets;
/// tiers 1–2 cover everyone who installs the binary themselves.
///
/// Asset names are expected to embed the Rust target triple and be
/// gzipped tarballs containing the `ucracc-lsp` binary at the archive root,
/// e.g. `ucracc-lsp-<version>-x86_64-unknown-linux-gnu.tar.gz`.
const RELEASE_REPO: &str = "crabcc-labs/zed-crabcc";

#[derive(Default)]
struct CrabccExtension {
    /// Path to a binary we downloaded earlier this session, cached so we
    /// don't re-hit the GitHub API on every `language_server_command`.
    cached_binary_path: Option<String>,
}

impl CrabccExtension {
    /// Tiers 1+2: an explicit `binary.path` override, then the worktree's
    /// host `$PATH`.
    ///
    /// This is evaluated **per-worktree** (never cached across worktrees):
    /// `worktree.which` resolves against *that worktree's* host, so a local
    /// project and a remote SSH project in the same Zed session each get the
    /// binary that actually exists on their respective host.
    fn resolve_on_host(worktree: &Worktree) -> Option<String> {
        if let Some(path) = LspSettings::for_worktree(SERVER_BINARY, worktree)
            .ok()
            .and_then(|s| s.binary)
            .and_then(|b| b.path)
        {
            return Some(path);
        }
        worktree.which(SERVER_BINARY)
    }

    /// Tier 3: download a prebuilt binary from [`RELEASE_REPO`] into the
    /// extension's (wasi-sandboxed) work dir and return a path to the
    /// extracted, executable `ucracc-lsp`. Cached for the session.
    fn download_binary(&mut self, id: &LanguageServerId) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
        }

        zed::set_language_server_installation_status(
            id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let release = zed::latest_github_release(
            RELEASE_REPO,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (os, arch) = zed::current_platform();
        let triple = target_triple(os, arch)?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name.contains(&triple) && a.name.ends_with(".tar.gz"))
            .ok_or_else(|| {
                format!(
                    "no `{SERVER_BINARY}` release asset for `{triple}` in {RELEASE_REPO} {}",
                    release.version
                )
            })?;

        // Versioned dir so an upgrade lands beside the old one; we GC the
        // stragglers after a successful extract.
        let dir = format!("{SERVER_BINARY}-{}", release.version);
        let bin = format!("{dir}/{SERVER_BINARY}");
        if !std::fs::metadata(&bin).is_ok_and(|m| m.is_file()) {
            zed::set_language_server_installation_status(
                id,
                &LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(&asset.download_url, &dir, DownloadedFileType::GzipTar)
                .map_err(|e| format!("downloading {} failed: {e}", asset.name))?;
            zed::make_file_executable(&bin)?;

            if let Ok(entries) = std::fs::read_dir(".") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(&format!("{SERVER_BINARY}-")) && name != dir {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }

        self.cached_binary_path = Some(bin.clone());
        Ok(bin)
    }

    fn binary_path(&mut self, id: &LanguageServerId, worktree: &Worktree) -> Result<String> {
        if let Some(path) = Self::resolve_on_host(worktree) {
            return Ok(path);
        }
        // Not on the host — fall back to a prebuilt download (no-op until the
        // public release repo publishes binaries). On failure, surface the
        // actionable manual-install hint rather than the raw download error.
        self.download_binary(id).map_err(|e| {
            format!(
                "`{SERVER_BINARY}` was not found on $PATH and auto-download failed ({e}). \
                 Install it (`cargo install ucracc-lsp`, or grab a crabcc release), then run \
                 `crabcc index` in the project root — or set `lsp.{SERVER_BINARY}.binary.path` \
                 in your Zed settings."
            )
        })
    }
}

/// Map Zed's `(Os, Architecture)` to the Rust target triple embedded in
/// release asset names.
fn target_triple(os: Os, arch: Architecture) -> Result<String> {
    let triple = match (os, arch) {
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
        (os, arch) => {
            return Err(format!(
                "no prebuilt `{SERVER_BINARY}` for {os:?}/{arch:?}; install it manually or set \
                 `lsp.{SERVER_BINARY}.binary.path`"
            ))
        }
    };
    Ok(triple.to_string())
}

impl zed::Extension for CrabccExtension {
    fn new() -> Self {
        Self::default()
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

        Ok(Command { command, args, env })
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
