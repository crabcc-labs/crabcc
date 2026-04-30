//! Single-file YAML config at `$HOME/.crabcc/._config.internal` —
//! issue #105 / #109 runtime defaults consumed by the agent runtime,
//! the ollama_stack driver, and the jobs module.
//!
//! ## Path resolution (highest precedence first)
//!
//! 1. Explicit path passed to [`load`] / [`load_or_default`].
//! 2. `$CRABCC_CONFIG` env var.
//! 3. `$HOME/.crabcc/._config.internal` — default. The leading `._`
//!    keeps it sorted out of the way of user-edited files; the
//!    `.internal` suffix flags it as managed-by-tool.
//!
//! ## Authoring
//!
//! `crabcc install-claude` writes a default config. Hand-edits are
//! supported — the file is plain YAML — but `crabcc upgrade` may
//! re-emit unknown sections to merge new defaults. Persist
//! customizations under sections that exist; don't introduce new
//! top-level keys (they'll be silently dropped on the next round-trip).
//!
//! ## Example
//!
//! ```yaml
//! agent:
//!   backend: claude        # claude | ollama
//!   default_model:
//!     claude: claude-opus-4-7
//!     ollama: ollama/qwen2.5-coder
//!
//! ollama:
//!   base_url: http://localhost:4000
//!   api_key_path: ~/.crabcc.local.api-key
//!
//! jobs:
//!   redis_url: redis://127.0.0.1:6379
//!
//! mcp:
//!   dev_surface: false
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const ENV_OVERRIDE: &str = "CRABCC_CONFIG";
const DEFAULT_REL_PATH: &str = ".crabcc/._config.internal";
const TRACE_TARGET: &str = "crabcc_core::config";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub agent: AgentConfig,
    pub ollama: OllamaConfig,
    pub jobs: JobsConfig,
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// `"claude"` or `"ollama"`. Mirrors the `--backend` CLI flag.
    pub backend: String,
    pub default_model: AgentDefaultModel,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            backend: "claude".into(),
            default_model: AgentDefaultModel::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentDefaultModel {
    pub claude: String,
    pub ollama: String,
}

impl Default for AgentDefaultModel {
    fn default() -> Self {
        Self {
            claude: "claude-opus-4-7".into(),
            // Apple Silicon optimized — Qwen3.5-35B-A3B MoE, 3B active/token.
            ollama: "ollama/qwen3.5:35b-a3b-coding-nvfp4".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OllamaConfig {
    pub base_url: String,
    pub api_key_path: String,
    /// Context window tokens. Qwen3.5-35B-A3B supports 262144 natively.
    /// ENV override: OLLAMA_NUM_CTX
    pub num_ctx: u32,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:4000".into(),
            api_key_path: "~/.crabcc.local.api-key".into(),
            num_ctx: 262144,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    pub redis_url: String,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            redis_url: "redis://127.0.0.1:6379".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub dev_surface: bool,
}

// ---------------------------------------------------------------------
// public surface
// ---------------------------------------------------------------------

/// Apply ENV var overrides on top of a loaded (or default) config.
///
/// Priority: ENV var > YAML config value > compiled default.
/// Recognised vars (all optional):
///   OLLAMA_BASE_URL   — overrides `ollama.base_url`
///   OLLAMA_NUM_CTX    — overrides `ollama.num_ctx` (parse as u32)
///   CRABCC_OLLAMA_MODEL — overrides `agent.default_model.ollama`
///   CRABCC_AGENT_BACKEND — overrides `agent.backend`
pub fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(v) = std::env::var("OLLAMA_BASE_URL") {
        if !v.is_empty() {
            cfg.ollama.base_url = v;
        }
    }
    if let Ok(v) = std::env::var("OLLAMA_NUM_CTX") {
        if let Ok(n) = v.parse::<u32>() {
            cfg.ollama.num_ctx = n;
        }
    }
    if let Ok(v) = std::env::var("CRABCC_OLLAMA_MODEL") {
        if !v.is_empty() {
            cfg.agent.default_model.ollama = v;
        }
    }
    if let Ok(v) = std::env::var("CRABCC_AGENT_BACKEND") {
        if !v.is_empty() {
            cfg.agent.backend = v;
        }
    }
}

/// Load with ENV overrides applied — the typical call site.
pub fn load_with_env(explicit: Option<&Path>) -> Config {
    let mut cfg = load_or_default(explicit);
    apply_env_overrides(&mut cfg);
    cfg
}

/// Resolve the config path in priority order. Pure — no I/O.
pub fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(p) = std::env::var(ENV_OVERRIDE) {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("$HOME not set; cannot resolve default config path")?;
    Ok(home.join(DEFAULT_REL_PATH))
}

/// Load + parse the config at the resolved path. Errors when the file
/// is missing OR contains unknown keys.
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    let path = resolve_path(explicit)?;
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read config {}", path.display()))?;
    let cfg: Config = serde_yml::from_str(&body)
        .with_context(|| format!("parse YAML config {}", path.display()))?;
    tracing::debug!(
        target: TRACE_TARGET,
        path = %path.display(),
        "config loaded"
    );
    Ok(cfg)
}

/// Load if present, otherwise return [`Config::default`]. Never errors
/// on a missing file — the default config IS the contract.
pub fn load_or_default(explicit: Option<&Path>) -> Config {
    match load(explicit) {
        Ok(c) => c,
        Err(_) => {
            tracing::debug!(
                target: TRACE_TARGET,
                "config not found / unparseable — returning Config::default()"
            );
            Config::default()
        }
    }
}

/// Atomically write the config as YAML. Creates parent dirs as needed.
/// Mode 0600 on Unix — the file embeds the Redis URL and Ollama API
/// key path; treat it as semi-secret.
pub fn save(explicit: Option<&Path>, cfg: &Config) -> Result<PathBuf> {
    let path = resolve_path(explicit)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_yml::to_string(cfg).context("serialize config to YAML")?;
    let banner = "# crabcc internal config — managed by `crabcc install-claude`.\n\
                  # Hand-edits supported. New keys may be added by future versions;\n\
                  # remove this banner only if you also strip the `# managed` marker.\n\n";
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{banner}{body}"))
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&tmp, perm);
    }
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    tracing::info!(
        target: TRACE_TARGET,
        path = %path.display(),
        "config saved"
    );
    Ok(path)
}

// ---------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_config_round_trips_through_yaml() {
        let cfg = Config::default();
        let yaml = serde_yml::to_string(&cfg).unwrap();
        let back: Config = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn default_values_match_phase4_constants() {
        let cfg = Config::default();
        assert_eq!(cfg.agent.backend, "claude");
        assert_eq!(cfg.agent.default_model.claude, "claude-opus-4-7");
        assert_eq!(
            cfg.agent.default_model.ollama,
            "ollama/qwen3.5:35b-a3b-coding-nvfp4"
        );
        assert_eq!(cfg.ollama.base_url, "http://localhost:4000");
        assert_eq!(cfg.jobs.redis_url, "redis://127.0.0.1:6379");
        assert!(!cfg.mcp.dev_surface);
    }

    #[test]
    fn explicit_path_wins_over_env_and_default() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("custom.yaml");
        let resolved = resolve_path(Some(&path)).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn save_then_load_round_trip() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("cfg.yaml");
        let mut cfg = Config::default();
        cfg.agent.backend = "ollama".into();
        cfg.jobs.redis_url = "redis://example.com:6379".into();

        let saved = save(Some(&path), &cfg).unwrap();
        assert_eq!(saved, path);
        assert!(path.exists());

        let loaded = load(Some(&path)).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn load_or_default_falls_back_on_missing_file() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("does-not-exist.yaml");
        let cfg = load_or_default(Some(&path));
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn unknown_top_level_keys_are_rejected() {
        let yaml = "agent:\n  backend: claude\nbogus_section:\n  foo: 1\n";
        let r: std::result::Result<Config, _> = serde_yml::from_str(yaml);
        assert!(r.is_err(), "deny_unknown_fields should reject");
    }

    #[test]
    fn save_writes_banner_comment() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("cfg.yaml");
        save(Some(&path), &Config::default()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# crabcc internal config"));
        assert!(body.contains("Hand-edits supported"));
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_0600_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let path = td.path().join("cfg.yaml");
        save(Some(&path), &Config::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // ---- apply_env_overrides ----
    //
    // These tests mutate process-wide env vars; run them under a lock to
    // avoid races with other tests that read the same vars.

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(pairs: &[(&str, &str)], f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Stash originals.
        let saved: Vec<(String, Option<String>)> = pairs
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        // Apply.
        for (k, v) in pairs {
            std::env::set_var(k, v);
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Restore.
        for (k, maybe_v) in &saved {
            match maybe_v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        if let Err(e) = r {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn apply_env_overrides_base_url() {
        with_env(&[("OLLAMA_BASE_URL", "http://custom:9999")], || {
            let mut cfg = Config::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.ollama.base_url, "http://custom:9999");
        });
    }

    #[test]
    fn apply_env_overrides_num_ctx() {
        with_env(&[("OLLAMA_NUM_CTX", "4096")], || {
            let mut cfg = Config::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.ollama.num_ctx, 4096);
        });
    }

    #[test]
    fn apply_env_overrides_ollama_model() {
        with_env(&[("CRABCC_OLLAMA_MODEL", "ollama/qwen2.5-coder")], || {
            let mut cfg = Config::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.agent.default_model.ollama, "ollama/qwen2.5-coder");
        });
    }

    #[test]
    fn apply_env_overrides_agent_backend() {
        with_env(&[("CRABCC_AGENT_BACKEND", "ollama")], || {
            let mut cfg = Config::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.agent.backend, "ollama");
        });
    }

    #[test]
    fn apply_env_overrides_empty_string_is_ignored() {
        // Empty values must not clobber the compiled default.
        with_env(&[("OLLAMA_BASE_URL", "")], || {
            let mut cfg = Config::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.ollama.base_url, "http://localhost:4000");
        });
    }

    #[test]
    fn apply_env_overrides_invalid_num_ctx_is_ignored() {
        // Non-numeric OLLAMA_NUM_CTX must silently fall through.
        with_env(&[("OLLAMA_NUM_CTX", "not-a-number")], || {
            let mut cfg = Config::default();
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.ollama.num_ctx, 262144);
        });
    }
}
