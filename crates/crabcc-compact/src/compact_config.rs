use crate::types::TransformKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactConfig {
    #[serde(default = "CompactConfig::default_rules")]
    pub enabled_rules: Vec<TransformKind>,
    #[serde(default = "CompactConfig::default_min_savings")]
    pub min_token_savings: u32,
    #[serde(default = "CompactConfig::default_max_disruption")]
    pub max_disruption: f64,
    #[serde(default)]
    pub per_project: HashMap<String, CompactConfig>,
}

impl CompactConfig {
    fn default_rules() -> Vec<TransformKind> {
        vec![]
    }
    fn default_min_savings() -> u32 {
        0
    }
    fn default_max_disruption() -> f64 {
        1.0
    }

    pub fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let candidate = path.unwrap_or_else(default_config_path);
        if !candidate.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&candidate)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", candidate.display()))?;
        serde_yml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", candidate.display()))
    }

    pub fn for_project<'a>(&'a self, project: &str) -> &'a Self {
        self.per_project.get(project).unwrap_or(self)
    }
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            enabled_rules: vec![],
            min_token_savings: 0,
            max_disruption: 1.0,
            per_project: HashMap::new(),
        }
    }
}

fn default_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".crabcc").join("compact.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_falls_back_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("does_not_exist.yaml");
        let cfg = CompactConfig::load(Some(nonexistent)).unwrap();
        assert_eq!(cfg.min_token_savings, 0);
        assert_eq!(cfg.max_disruption, 1.0);
        assert!(cfg.enabled_rules.is_empty());
    }

    #[test]
    fn parse_yaml_sets_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compact.yaml");
        std::fs::write(&path, "min_token_savings: 50\n").unwrap();
        let cfg = CompactConfig::load(Some(path)).unwrap();
        assert_eq!(cfg.min_token_savings, 50);
    }
}
