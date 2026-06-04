//! Per-model metadata at `$CRABCC_HOME/models/.model.<provider>.<name>.info`.
//!
//! What the file holds:
//!   - `provider`     ollama / claude / openai / litellm
//!   - `name`         the actual model id passed to the runtime
//!   - `params`       the headline parameter count / quantization
//!   - `context`      max context window in tokens
//!   - `flags`        list of CLI flags / API params the model accepts
//!     plus a one-line description of each
//!   - `docs`         official documentation URL(s)
//!   - `notes`        free-form per-installation notes (rate limits, licence, local quirks)
//!
//! Used by:
//!   - `crabcc model-info show <name>`  human / JSON dump for users
//!   - `crabcc model-info create ...`   write a fresh file
//!   - The agent banner — printed to stderr when `crabcc agent` starts
//!     against a backend whose default model has a matching .info file
//!   - `Containerfile` — `LABEL com.crabcc.model.info="..."` + a copy
//!     baked into the image at /etc/crabcc/.model.<...>.info
//!
//! The format is TOML so editor support is good and the file is also
//! valid as a Docker label value when serialized to one line.

#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub name: String,
    pub params: Option<String>,
    pub context: Option<u64>,
    #[serde(default)]
    pub flags: Vec<ModelFlag>,
    #[serde(default)]
    pub docs: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFlag {
    pub name: String,
    pub default: Option<String>,
    pub description: String,
}

/// Default location: `$CRABCC_HOME/models/` (or `~/.crabcc/models/` if
/// `CRABCC_HOME` is unset). Mirrors the `_internal.db` location pattern.
pub fn default_dir(home: &Path) -> PathBuf {
    if let Ok(crabcc_home) = std::env::var("CRABCC_HOME") {
        return PathBuf::from(crabcc_home).join("models");
    }
    home.join(".crabcc").join("models")
}

pub fn file_path(home: &Path, provider: &str, name: &str) -> PathBuf {
    // Slashes in the model name (e.g. `ollama/qwen2.5-coder`) get sanitized
    // to underscores so the on-disk filename is portable across all the
    // file systems we care about. The TOML body keeps the original.
    let safe_name = name.replace('/', "_");
    default_dir(home).join(format!(".model.{provider}.{safe_name}.info"))
}

pub fn write(home: &Path, info: &ModelInfo) -> Result<PathBuf> {
    let path = file_path(home, &info.provider, &info.name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = toml::to_string_pretty(info).context("serialize ModelInfo to TOML")?;
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn read(home: &Path, provider: &str, name: &str) -> Result<Option<ModelInfo>> {
    let path = file_path(home, provider, name);
    if !path.exists() {
        return Ok(None);
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let info: ModelInfo =
        toml::from_str(&body).with_context(|| format!("parse TOML from {}", path.display()))?;
    Ok(Some(info))
}

/// One-line banner printed to stderr when an agent starts against this
/// model. Designed to fit in a 120-char terminal: `<provider>:<name>
/// (context: 32k, docs: …)`.
pub fn banner_line(info: &ModelInfo) -> String {
    let ctx = info
        .context
        .map(|c| {
            if c >= 1024 {
                format!("{}k", c / 1024)
            } else {
                c.to_string()
            }
        })
        .unwrap_or_else(|| "?".into());
    let docs = info
        .docs
        .first()
        .map(|s| s.as_str())
        .unwrap_or("(no docs link)");
    format!(
        "model: {}:{} (params: {}, context: {}, docs: {})",
        info.provider,
        info.name,
        info.params.as_deref().unwrap_or("?"),
        ctx,
        docs
    )
}

/// Seed a default file for the bundled Ollama default
/// (qwen2.5-coder per agent.rs::DEFAULT_OLLAMA_MODEL). Idempotent;
/// returns early if the file already exists. Used by the install path
/// and the Containerfile bake step so agents always have some
/// metadata to print, even on a fresh dev box.
pub fn seed_default_ollama(home: &Path) -> Result<PathBuf> {
    let path = file_path(home, "ollama", "qwen2.5-coder");
    if path.exists() {
        return Ok(path);
    }
    let info = ModelInfo {
        provider: "ollama".into(),
        name: "qwen2.5-coder".into(),
        params: Some("7B (Q4_K_M default)".into()),
        context: Some(32768),
        flags: vec![
            ModelFlag {
                name: "--temperature".into(),
                default: Some("0.2".into()),
                description: "sampling temperature; lower = more deterministic for code edits"
                    .into(),
            },
            ModelFlag {
                name: "--num-ctx".into(),
                default: Some("32768".into()),
                description: "context window; raise for repo-wide refactors, watch RAM".into(),
            },
            ModelFlag {
                name: "--top-p".into(),
                default: Some("0.9".into()),
                description: "nucleus sampling cutoff".into(),
            },
            ModelFlag {
                name: "--repeat-penalty".into(),
                default: Some("1.05".into()),
                description: "discourage echo loops on long generations".into(),
            },
        ],
        docs: vec![
            "https://ollama.com/library/qwen2.5-coder".into(),
            "https://qwenlm.github.io/blog/qwen2.5-coder/".into(),
        ],
        notes: Some(
            "Bundled default for `crabcc agent --backend ollama`. Override with \
             --model ollama/<other>. The model is pulled on-demand by the \
             ollama-stack on first agent run — no manual `ollama pull` needed."
                .into(),
        ),
    };
    write(home, &info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_writes_and_reads_toml() {
        let tmp = tempdir().unwrap();
        let info = ModelInfo {
            provider: "ollama".into(),
            name: "qwen2.5-coder".into(),
            params: Some("7B".into()),
            context: Some(32768),
            flags: vec![ModelFlag {
                name: "--temperature".into(),
                default: Some("0.2".into()),
                description: "sampling temp".into(),
            }],
            docs: vec!["https://example.com".into()],
            notes: None,
        };
        let path = write(tmp.path(), &info).unwrap();
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".model."));
        let round = read(tmp.path(), "ollama", "qwen2.5-coder")
            .unwrap()
            .unwrap();
        assert_eq!(round.provider, "ollama");
        assert_eq!(round.flags.len(), 1);
        assert_eq!(round.context, Some(32768));
    }

    #[test]
    fn slash_in_model_name_is_sanitized_in_filename() {
        let tmp = tempdir().unwrap();
        let info = ModelInfo {
            provider: "litellm".into(),
            name: "ollama/qwen2.5-coder".into(),
            params: None,
            context: None,
            flags: vec![],
            docs: vec![],
            notes: None,
        };
        let path = write(tmp.path(), &info).unwrap();
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!fname.contains('/'));
        assert!(fname.contains("ollama_qwen2.5-coder"));
    }

    #[test]
    fn seed_default_ollama_is_idempotent() {
        let tmp = tempdir().unwrap();
        let p1 = seed_default_ollama(tmp.path()).unwrap();
        let p2 = seed_default_ollama(tmp.path()).unwrap();
        assert_eq!(p1, p2);
        let info = read(tmp.path(), "ollama", "qwen2.5-coder")
            .unwrap()
            .unwrap();
        assert!(info.context.unwrap() > 0);
        assert!(!info.flags.is_empty());
    }

    #[test]
    fn banner_line_includes_provider_name_and_context() {
        let info = ModelInfo {
            provider: "ollama".into(),
            name: "qwen2.5-coder".into(),
            params: Some("7B".into()),
            context: Some(32768),
            flags: vec![],
            docs: vec!["https://docs/".into()],
            notes: None,
        };
        let banner = banner_line(&info);
        assert!(banner.contains("ollama:qwen2.5-coder"));
        assert!(banner.contains("32k"));
        assert!(banner.contains("https://docs/"));
    }
}
