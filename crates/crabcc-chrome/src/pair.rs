//! Install / remove the Chrome NativeMessagingHosts manifest.
//!
//! Per Chrome's spec, the manifest is a JSON file whose path the browser
//! discovers from a per-platform location. We currently support macOS
//! and Linux — Windows uses registry keys and is left as a follow-up.
//!
//! macOS:   ~/Library/Application Support/<browser>/NativeMessagingHosts/<host_name>.json
//! Linux:   ~/.config/<browser>/NativeMessagingHosts/<host_name>.json
//!
//! See: https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging#native-messaging-host-location

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

use crate::{config, HOST_NAME};

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum Browser {
    Chrome,
    Chromium,
    Brave,
    Edge,
}

impl Browser {
    fn dir_segment(self) -> &'static str {
        match self {
            // The first segment varies per OS — see `manifest_dir` for
            // the fully-resolved path. This returns the browser-specific
            // tail.
            Browser::Chrome => "Google/Chrome",
            Browser::Chromium => "Chromium",
            Browser::Brave => "BraveSoftware/Brave-Browser",
            Browser::Edge => "Microsoft Edge",
        }
    }
    fn linux_segment(self) -> &'static str {
        match self {
            Browser::Chrome => "google-chrome",
            Browser::Chromium => "chromium",
            Browser::Brave => "BraveSoftware/Brave-Browser",
            Browser::Edge => "microsoft-edge",
        }
    }
}

#[derive(Debug, Serialize)]
struct Manifest {
    name: String,
    description: String,
    path: String,
    #[serde(rename = "type")]
    ty: String,
    allowed_origins: Vec<String>,
}

pub fn manifest_path(browser: Browser) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("neither $HOME nor %USERPROFILE% is set"))?;
    let home = PathBuf::from(home);
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
            .join(browser.dir_segment())
            .join("NativeMessagingHosts")
    } else if cfg!(target_os = "linux") {
        home.join(".config")
            .join(browser.linux_segment())
            .join("NativeMessagingHosts")
    } else {
        return Err(anyhow!("unsupported platform — install manually"));
    };
    Ok(dir.join(format!("{HOST_NAME}.json")))
}

pub fn install(extension_id: &str, browser: Browser, force: bool) -> Result<()> {
    validate_extension_id(extension_id)?;

    let bin = current_executable()?;
    if !bin.exists() {
        return Err(anyhow!(
            "the resolved binary path {} does not exist",
            bin.display()
        ));
    }

    let manifest = Manifest {
        name: HOST_NAME.into(),
        description: "crabcc Chrome bridge — native-messaging host".into(),
        path: bin.to_string_lossy().into_owned(),
        ty: "stdio".into(),
        // Chrome requires the chrome-extension://<id>/ form with the
        // trailing slash; brave/edge follow the same convention.
        allowed_origins: vec![format!("chrome-extension://{extension_id}/")],
    };

    let manifest_path = manifest_path(browser)?;
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if manifest_path.exists() && !force {
        return Err(anyhow!(
            "{} already exists — re-run with --force to overwrite",
            manifest_path.display()
        ));
    }
    let body = serde_json::to_string_pretty(&manifest).context("serialising manifest")?;
    fs::write(&manifest_path, body)
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    // Generate (or refresh) the shared secret in chrome.toml. The port
    // stays 0 until `serve` first runs.
    let mut cfg = config::load_or_default();
    if cfg.secret.is_empty() || force {
        cfg.secret = config::generate_secret();
    }
    cfg.extension_id = extension_id.to_string();
    config::save(&cfg).context("writing chrome.toml")?;

    println!("installed {}", manifest_path.display());
    println!("config:    {}", config::path()?.display());
    println!();
    println!("next: run `crabcc-chrome serve` from your MCP client config.");
    Ok(())
}

pub fn remove(browser: Browser) -> Result<()> {
    let manifest_path = manifest_path(browser)?;
    if manifest_path.exists() {
        fs::remove_file(&manifest_path)
            .with_context(|| format!("removing {}", manifest_path.display()))?;
        println!("removed {}", manifest_path.display());
    } else {
        println!("nothing to remove at {}", manifest_path.display());
    }
    let cfg_path = config::path()?;
    if cfg_path.exists() {
        fs::remove_file(&cfg_path).with_context(|| format!("removing {}", cfg_path.display()))?;
        println!("removed {}", cfg_path.display());
    }
    Ok(())
}

/// Resolve the absolute path of the currently running binary.
/// `std::env::current_exe()` returns a symlink target on macOS/Linux,
/// which is what Chrome wants in the manifest's `path` field.
fn current_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving current_exe")?;
    canonicalize_or(exe.clone(), exe)
}

fn canonicalize_or(p: PathBuf, fallback: PathBuf) -> Result<PathBuf> {
    Ok(fs::canonicalize(&p).unwrap_or(fallback))
}

fn validate_extension_id(id: &str) -> Result<()> {
    // Chrome extension IDs are lowercase a-p, exactly 32 chars long.
    // Reject anything else early so the operator finds out at pair time
    // rather than at first connectNative.
    if id.len() != 32 {
        return Err(anyhow!(
            "extension id must be exactly 32 characters; got {}",
            id.len()
        ));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() && (b'a'..=b'p').contains(&b))
    {
        return Err(anyhow!(
            "extension id must contain only lowercase letters a-p"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_extension_id_format() {
        assert!(validate_extension_id("abcdefghijklmnopabcdefghijklmnop").is_ok());
        assert!(validate_extension_id("abc").is_err());
        assert!(validate_extension_id("ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP").is_err());
        assert!(validate_extension_id("zbcdefghijklmnopabcdefghijklmnop").is_err());
    }

    #[test]
    fn manifest_path_includes_host_name() {
        if let Ok(p) = manifest_path(Browser::Chrome) {
            assert!(p.to_string_lossy().contains("com.crabcc.chrome.json"));
            assert!(p.to_string_lossy().contains("NativeMessagingHosts"));
        }
    }

    #[test]
    fn install_writes_manifest_and_secret() {
        // Share the env-var lock with config::tests so the two test
        // modules don't race on $CRABCC_CHROME_CONFIG / $HOME.
        let _g = crate::test_util::ENV_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("chrome.toml");
        std::env::set_var("CRABCC_CHROME_CONFIG", &cfg_path);
        // Override $HOME so manifest_path resolves under the temp dir.
        let home = dir.path().to_path_buf();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let id = "abcdefghijklmnopabcdefghijklmnop";
        install(id, Browser::Chrome, false).unwrap();

        assert!(cfg_path.exists(), "chrome.toml not written");
        let cfg = config::load_or_default();
        assert_eq!(cfg.secret.len(), 64);
        assert_eq!(cfg.extension_id, id);

        let mp = manifest_path(Browser::Chrome).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mp).unwrap()).unwrap();
        assert_eq!(body["name"], "com.crabcc.chrome");
        assert_eq!(
            body["allowed_origins"][0],
            format!("chrome-extension://{id}/")
        );

        std::env::remove_var("CRABCC_CHROME_CONFIG");
        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }
}
