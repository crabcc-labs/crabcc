//! Internal-agent profile loader (Ask B / issue #112 follow-up).
//!
//! When `crabcc agent --profile internal/<name>` runs, this module:
//!   1. Resolves `<repo>/internal_agents/<name>.profile.toml` and parses it.
//!   2. Composes the system prompt: `shared.agent.md` followed by `<name>.agent.md`.
//!   3. Hands the result back to `agent::run` which threads it through the
//!      existing `--append-system-prompt` flow.
//!
//! Per-crate repomix bundles are produced via `task repomix-crate
//! CRATE=<crate>` from the agent shell — this loader doesn't shell out
//! itself; it just composes prompts and exports env vars.
//
// dead_code allow: the public struct fields (description, max_iterations,
// timeout_secs, gates, tools.allowed, etc.) define the on-disk profile
// schema. Tests cover them; runtime consumers (system-prompt rendering,
// future GH-orchestration in #112 follow-ups) will read them. Removing
// fields would be a surface-breaking change.
#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Profile prefix recognised by `--profile`. Future namespaces
/// (e.g. `customer/foo`) drop in alongside this one.
pub const INTERNAL_PREFIX: &str = "internal/";

#[derive(Debug, Clone, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub crate_: Option<String>, // serde alias below maps the TOML "crate" key
    pub description: Option<String>,
    pub model: Option<String>,
    pub max_iterations: Option<u32>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub gates: GatesSection,
    /// Filled in by the loader after the on-disk parse.
    #[serde(skip)]
    pub system_prompt: String,
    #[serde(skip)]
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolsSection {
    #[serde(default)]
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GatesSection {
    #[serde(default)]
    pub bench: Vec<String>,
    #[serde(default)]
    pub test: Vec<String>,
    #[serde(default)]
    pub lint: Vec<String>,
    #[serde(default)]
    pub fmt: Vec<String>,
}

/// Strip the `internal/` prefix and return the bare profile name.
/// Anything else returns None — namespace gating is centralized so a
/// future `customer/<name>` form has one place to slot into.
pub fn parse_internal_profile_id(profile: &str) -> Option<&str> {
    profile.strip_prefix(INTERNAL_PREFIX)
}

/// Default profile when the user passes `--profile internal/` (no name)
/// or any other ambiguous form. Errors when even this fallback is
/// missing rather than silently running without a profile.
pub const DEFAULT_PROFILE_ID: &str = "default";

/// Load `<repo_root>/internal_agents/<name>.profile.toml` plus the
/// matching `.agent.md` and the shared preamble. Errors hard if the
/// requested profile (or the `default` fallback) doesn't exist —
/// silently downgrading to no profile would mask typos.
pub fn load(repo_root: &Path, profile_id: &str) -> Result<AgentProfile> {
    if profile_id.is_empty() || profile_id.contains('/') || profile_id.contains("..") {
        anyhow::bail!("invalid profile id '{profile_id}' — names must match [a-z0-9-]+");
    }
    let dir = repo_root.join("internal_agents");
    let toml_path = dir.join(format!("{profile_id}.profile.toml"));
    let prompt_path = dir.join(format!("{profile_id}.agent.md"));
    let shared_path = dir.join("shared.agent.md");

    if !toml_path.exists() {
        let available = list_available_profiles(&dir).unwrap_or_default();
        anyhow::bail!(
            "internal-agent profile '{profile_id}' not found at {}\nAvailable: {}",
            toml_path.display(),
            if available.is_empty() {
                "(none — internal_agents/ is empty)".into()
            } else {
                available.join(", ")
            }
        );
    }
    let toml_body = std::fs::read_to_string(&toml_path)
        .with_context(|| format!("read profile {}", toml_path.display()))?;

    // Parse with the `crate` key remapped to `crate_` (Rust keyword).
    let mut profile: AgentProfile = parse_profile_toml(&toml_body, &toml_path)?;

    let shared = std::fs::read_to_string(&shared_path)
        .with_context(|| format!("read shared preamble {}", shared_path.display()))?;
    let crate_specific = std::fs::read_to_string(&prompt_path)
        .with_context(|| format!("read crate prompt {}", prompt_path.display()))?;
    profile.system_prompt = format!("{shared}\n\n---\n\n{crate_specific}");
    profile.source_path = toml_path;
    Ok(profile)
}

/// Enumerate available profile ids (filenames matching `*.profile.toml`)
/// for nicer error messages on typos.
fn list_available_profiles(dir: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            name.to_string_lossy()
                .strip_suffix(".profile.toml")
                .map(|s| s.to_string())
        })
        .collect();
    out.sort();
    Ok(out)
}

fn parse_profile_toml(body: &str, source: &Path) -> Result<AgentProfile> {
    // The TOML key is `crate`, which is a reserved Rust ident — rewrite to
    // `crate_` before deserializing. Cheap on bytes; small files.
    let body = body.replacen("\ncrate ", "\ncrate_ ", 1);
    let body = body.replacen("\ncrate=", "\ncrate_=", 1);
    let body = if body.starts_with("crate ") || body.starts_with("crate=") {
        body.replacen("crate", "crate_", 1)
    } else {
        body
    };
    toml::from_str(&body).with_context(|| format!("parse {}", source.display()))
}

impl AgentProfile {
    /// All env vars to export to the spawned agent's child process.
    pub fn env_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.env.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Default model, falling back to crabcc-cli's DEFAULT_MODEL when the
    /// profile didn't specify one.
    pub fn model_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.model.as_deref().unwrap_or(fallback)
    }

    /// Bench / test / lint / fmt commands the agent must keep green.
    /// Concatenated as a single Vec for the system-prompt rendering;
    /// callers that need them split should access the GatesSection
    /// fields directly.
    pub fn all_gate_commands(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(
            self.gates.bench.len()
                + self.gates.test.len()
                + self.gates.lint.len()
                + self.gates.fmt.len(),
        );
        out.extend(self.gates.bench.iter().map(|s| s.as_str()));
        out.extend(self.gates.test.iter().map(|s| s.as_str()));
        out.extend(self.gates.lint.iter().map(|s| s.as_str()));
        out.extend(self.gates.fmt.iter().map(|s| s.as_str()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skeleton(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join("internal_agents")).unwrap();
        fs::write(
            dir.join("internal_agents").join("shared.agent.md"),
            "# Shared\n\nFollow the workflow.\n",
        )
        .unwrap();
        fs::write(
            dir.join("internal_agents").join(format!("{name}.agent.md")),
            format!("# {name} specialist\n\nYou own crates/{name}/.\n"),
        )
        .unwrap();
        fs::write(
            dir.join("internal_agents")
                .join(format!("{name}.profile.toml")),
            format!(
                r#"name = "{name}"
crate = "{name}"
model = "claude-opus-4-7"
max_iterations = 10
timeout_secs = 600

[env]
CRABCC_BUILD_PROFILE = "release-native"
RUST_LOG = "crabcc_core=info"

[tools]
allowed = ["Bash", "Read", "mcp__crabcc__sym"]

[gates]
test = ["cargo test -p {name} --release"]
lint = ["cargo clippy -p {name} -- -D warnings"]
"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn parse_internal_prefix_only_accepts_internal_namespace() {
        assert_eq!(
            parse_internal_profile_id("internal/crabcc-core"),
            Some("crabcc-core")
        );
        assert_eq!(parse_internal_profile_id("crabcc-core"), None);
        assert_eq!(parse_internal_profile_id("customer/foo"), None);
    }

    #[test]
    fn load_composes_shared_preamble_with_crate_prompt() {
        let dir = tempdir().unwrap();
        write_skeleton(dir.path(), "crabcc-core");
        let p = load(dir.path(), "crabcc-core").unwrap();
        assert_eq!(p.name, "crabcc-core");
        assert!(p.system_prompt.contains("# Shared"));
        assert!(p.system_prompt.contains("crabcc-core specialist"));
        assert_eq!(p.model_or("fallback"), "claude-opus-4-7");
        assert_eq!(p.gates.test.len(), 1);
        assert_eq!(p.tools.allowed.len(), 3);
        assert_eq!(
            p.env.get("CRABCC_BUILD_PROFILE").map(|s| s.as_str()),
            Some("release-native")
        );
    }

    #[test]
    fn load_rejects_path_traversal() {
        let dir = tempdir().unwrap();
        write_skeleton(dir.path(), "crabcc-core");
        assert!(load(dir.path(), "../../etc/passwd").is_err());
        assert!(load(dir.path(), "internal/crabcc-core").is_err());
        assert!(load(dir.path(), "").is_err());
    }

    #[test]
    fn all_gate_commands_concatenates_in_canonical_order() {
        let dir = tempdir().unwrap();
        write_skeleton(dir.path(), "crabcc-core");
        let p = load(dir.path(), "crabcc-core").unwrap();
        let cmds = p.all_gate_commands();
        // Order: bench, test, lint, fmt. Profile only has test + lint.
        assert!(cmds.iter().any(|c| c.contains("cargo test")));
        assert!(cmds.iter().any(|c| c.contains("clippy")));
    }
}
