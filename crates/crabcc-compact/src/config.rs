use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub threshold_tokens: usize,
    pub timeout_ms: u64,
    pub enrich_trigger: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            threshold_tokens: 2000,
            timeout_ms: 8000,
            enrich_trigger: "!e".to_string(),
        }
    }
}

pub fn load() -> anyhow::Result<Config> {
    let local = PathBuf::from(".crabcc/compact.toml");
    if local.exists() {
        return parse_file(&local);
    }
    let user = dirs_path().join("compact.toml");
    if user.exists() {
        return parse_file(&user);
    }
    Ok(Config::default())
}

fn dirs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("crabcc")
}

fn parse_file(path: &PathBuf) -> anyhow::Result<Config> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    toml::from_str(&raw).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let c = Config::default();
        assert_eq!(c.threshold_tokens, 2000);
        assert_eq!(c.timeout_ms, 8000);
        assert_eq!(c.enrich_trigger, "!e");
    }

    #[test]
    fn parse_toml_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compact.toml");
        std::fs::write(
            &path,
            r#"
endpoint = "https://compact.example.ts.net:8080"
threshold_tokens = 3000
timeout_ms = 5000
enrich_trigger = "!enrich"
"#,
        )
        .unwrap();
        let c: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.endpoint, "https://compact.example.ts.net:8080");
        assert_eq!(c.threshold_tokens, 3000);
        assert_eq!(c.enrich_trigger, "!enrich");
    }
}
