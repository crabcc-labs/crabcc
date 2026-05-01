//! Shared config + secret between `host` and `serve` modes.
//!
//! Stored at `~/.crabcc/chrome.toml`, mode 0600 on Unix. Three fields:
//!
//! - `port` — TCP loopback port `serve` is bound to. Written by `serve`
//!   on startup (after binding 127.0.0.1:0 and reading back the port);
//!   read by `host` on launch.
//! - `secret` — random 32-byte hex token. Written by `pair`, presented
//!   by `host` on connect, validated by `serve`. The IPC socket is
//!   loopback-only but a malicious local process could still scan it —
//!   the secret pins which process is authorised.
//! - `extension_id` — Chrome extension ID, recorded for diagnostics.
//!
//! The file format is intentionally TOML rather than JSON: it's edited
//! by humans during pairing and tolerates comments + trailing newlines.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// TCP port `serve` is listening on. 0 means "not currently up".
    #[serde(default)]
    pub port: u16,
    /// Hex-encoded random secret. Empty until `pair` runs.
    #[serde(default)]
    pub secret: String,
    /// Chrome extension ID (32-char hash). For diagnostics; the manifest
    /// is the source of truth for `allowed_origins`.
    #[serde(default)]
    pub extension_id: String,
}

/// Returns `~/.crabcc/chrome.toml`. Honors `$CRABCC_CHROME_CONFIG` for
/// tests and unusual deployments; otherwise falls back to `$HOME` /
/// `%USERPROFILE%`.
pub fn path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CRABCC_CHROME_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("neither $HOME nor %USERPROFILE% is set")?;
    Ok(PathBuf::from(home).join(".crabcc").join("chrome.toml"))
}

/// Read the config file. Returns `Config::default()` if the file is
/// missing — callers should fail loudly downstream when they need a
/// non-empty secret or non-zero port.
pub fn load_or_default() -> Config {
    let p = match path() {
        Ok(p) => p,
        Err(_) => return Config::default(),
    };
    fs::read_to_string(&p)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write `cfg` atomically: write to a temp file in the same directory,
/// then rename. Sets mode 0600 on Unix.
pub fn save(cfg: &Config) -> Result<()> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = p.with_extension("toml.tmp");
    let body = toml::to_string_pretty(cfg).context("serialising config")?;
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&tmp, perms)?;
    }
    fs::rename(&tmp, &p).with_context(|| format!("renaming into {}", p.display()))?;
    Ok(())
}

/// Generate a fresh 32-byte random hex secret.
///
/// Uses SHA-256 over (process id, thread id, monotonic time, an
/// OS-supplied random sample if available). This is intentionally not a
/// cryptographic-grade RNG: the threat model is "another local user
/// scans loopback ports", and 256 bits of unpredictability covers that.
/// We avoid pulling a `rand` dependency — keeping dep count low matches
/// the rest of the workspace.
pub fn generate_secret() -> String {
    let mut h = Sha256::new();
    let pid = std::process::id();
    h.update(pid.to_le_bytes());
    let tid = format!("{:?}", std::thread::current().id());
    h.update(tid.as_bytes());
    if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
        h.update(d.as_nanos().to_le_bytes());
    }
    // /dev/urandom on Unix; ignored on Windows (PID + nanos remain).
    if let Ok(mut buf) = read_urandom(32) {
        h.update(&buf);
        buf.fill(0);
    }
    let digest = h.finalize();
    hex(&digest)
}

#[cfg(unix)]
fn read_urandom(n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(not(unix))]
fn read_urandom(_: usize) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "/dev/urandom not available",
    ))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ENV_GUARD;

    #[test]
    fn generate_secret_produces_64_hex_chars() {
        let s = generate_secret();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_secret_is_unique_across_calls() {
        // Strict equality is what we want here — two consecutive calls
        // mixing pid/tid/nanos/urandom must produce distinct digests.
        let a = generate_secret();
        let b = generate_secret();
        assert_ne!(a, b);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let _g = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("chrome.toml");
        std::env::set_var("CRABCC_CHROME_CONFIG", &p);
        let cfg = Config {
            port: 41234,
            secret: "abc123".into(),
            extension_id: "iddqd".into(),
        };
        save(&cfg).unwrap();
        let loaded = load_or_default();
        assert_eq!(loaded.port, 41234);
        assert_eq!(loaded.secret, "abc123");
        assert_eq!(loaded.extension_id, "iddqd");
        std::env::remove_var("CRABCC_CHROME_CONFIG");
    }
}
