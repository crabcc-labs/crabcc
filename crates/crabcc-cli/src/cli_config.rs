//! `.crabcc-cli.conf` — the agent-shell rewrite + compaction defaults,
//! made visible and overridable in one place.
//!
//! Precedence: **environment variable > this file > built-in default.**
//! The `CRABCC_NO_*` env flags are disable-only escape hatches and always
//! win; the config file is where the sane on-by-default knobs live; absent
//! a file (or a key), the built-in defaults apply. Every key here is wired
//! into [`crate::shell_rewrite`] — there are no decorative toggles.

use serde::Deserialize;
use std::path::Path;

/// Resolved rewrite/compaction config (defaults = the shipped behaviour).
#[derive(Debug, Clone, PartialEq)]
pub struct CliConfig {
    /// Master switch for the PreToolUse Bash rewrite engine.
    pub rewrite_enabled: bool,
    /// Local RTK filter stage (lossless, command-aware).
    pub rtk: bool,
    /// Morph compaction stage (still gated on `MORPH_API_KEY`).
    pub morph: bool,
    /// Min residual bytes before paying Morph's network round-trip.
    pub morph_min_bytes: usize,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            rewrite_enabled: true,
            rtk: true,
            morph: true,
            morph_min_bytes: 8000,
        }
    }
}

#[derive(Deserialize, Default)]
struct RawConfig {
    rewrite: Option<RawRewrite>,
    compact: Option<RawCompact>,
}
#[derive(Deserialize, Default)]
struct RawRewrite {
    enabled: Option<bool>,
}
#[derive(Deserialize, Default)]
struct RawCompact {
    rtk: Option<bool>,
    morph: Option<bool>,
    morph_min_bytes: Option<usize>,
}

/// Load `<root>/.crabcc-cli.conf`, falling back to [`CliConfig::default`]
/// for a missing file, unreadable file, or malformed TOML (best-effort —
/// a broken config never breaks the rewrite path, it just uses defaults).
pub fn load(root: &Path) -> CliConfig {
    let mut cfg = CliConfig::default();
    let Ok(text) = std::fs::read_to_string(root.join(".crabcc-cli.conf")) else {
        return cfg;
    };
    let Ok(raw) = toml::from_str::<RawConfig>(&text) else {
        return cfg;
    };
    if let Some(enabled) = raw.rewrite.and_then(|r| r.enabled) {
        cfg.rewrite_enabled = enabled;
    }
    if let Some(c) = raw.compact {
        if let Some(v) = c.rtk {
            cfg.rtk = v;
        }
        if let Some(v) = c.morph {
            cfg.morph = v;
        }
        if let Some(v) = c.morph_min_bytes {
            cfg.morph_min_bytes = v;
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_file_yields_sane_defaults() {
        let dir = tempdir().unwrap();
        assert_eq!(load(dir.path()), CliConfig::default());
        // Defaults are the perf-focused on-by-default set.
        let d = CliConfig::default();
        assert!(d.rewrite_enabled && d.rtk && d.morph);
        assert_eq!(d.morph_min_bytes, 8000);
    }

    #[test]
    fn parses_overrides_and_keeps_unset_keys_at_default() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".crabcc-cli.conf"),
            "[compact]\nmorph = false\nmorph_min_bytes = 12000\n",
        )
        .unwrap();
        let cfg = load(dir.path());
        assert!(!cfg.morph, "morph disabled by file");
        assert_eq!(cfg.morph_min_bytes, 12000);
        assert!(cfg.rewrite_enabled && cfg.rtk, "unset keys stay default");
    }

    #[test]
    fn malformed_toml_falls_back_to_defaults() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".crabcc-cli.conf"), "this = is = not toml").unwrap();
        assert_eq!(load(dir.path()), CliConfig::default());
    }
}
