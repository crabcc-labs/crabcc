//! Host-info collection. Everything in this module is PII-clean by
//! construction:
//!
//!   * `hostname_hash` — SHA-256 of the kernel-reported hostname,
//!     truncated to 16 hex chars. Stable across boots, NOT
//!     reversible to the original hostname.
//!   * `machine_id_hash` — SHA-256 of the platform machine UUID
//!     (`/etc/machine-id` on Linux, `IOPlatformUUID` on macOS),
//!     same 16-char prefix.
//!
//! Why hash and not omit? Hashing lets us correlate crash reports
//! across runs from the same machine without ever surfacing the raw
//! identifier — useful when the user opts in to opening a GH issue
//! via `crabcc-godfather gh-issue` and we want to show "this is your
//! 3rd crash today" without leaking who "you" are.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One-row representation of `_crab_host`. Re-collected on every
/// `Godfather::open`; kernel-reported values are cheap and we want
/// `os_version` etc. to refresh after an OS upgrade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu_count: i64,
    pub total_memory_mb: i64,
    pub hostname_hash: String,
    pub machine_id_hash: String,
}

impl HostInfo {
    /// Collect via `sysinfo` + the platform machine-ID file. Never
    /// surfaces a raw hostname or UUID to the caller.
    pub fn collect() -> Self {
        use sysinfo::System;
        let mut sys = System::new();
        sys.refresh_memory();
        Self {
            os: System::name().unwrap_or_else(|| "unknown".into()),
            os_version: System::os_version().unwrap_or_else(|| "unknown".into()),
            arch: std::env::consts::ARCH.to_string(),
            cpu_count: sys.cpus().len().max(num_cpus_fallback()) as i64,
            total_memory_mb: (sys.total_memory() / 1024 / 1024) as i64,
            hostname_hash: hash16(&System::host_name().unwrap_or_default()),
            machine_id_hash: hash16(&machine_id().unwrap_or_default()),
        }
    }
}

/// Read `/etc/machine-id` (Linux) / `IOPlatformUUID` (macOS) /
/// `MachineGuid` registry key (Windows). Returns an empty string on
/// platforms that don't expose one — `hash16("")` becomes a stable
/// well-known hash (the SHA-256 of empty), so anonymous-mode runs
/// still cluster together rather than scattering across noise.
fn machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/machine-id")
            .ok()
            .map(|s| s.trim().to_string())
    }
    #[cfg(target_os = "macos")]
    {
        // `ioreg -rd1 -c IOPlatformExpertDevice` is the canonical
        // path; we shell out because pulling `core-foundation` just
        // for one ID would dwarf the whole crate.
        let out = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let s = String::from_utf8(out.stdout).ok()?;
        for line in s.lines() {
            if let Some(rest) = line.split_once("\"IOPlatformUUID\" = \"") {
                if let Some(end) = rest.1.find('"') {
                    return Some(rest.1[..end].to_string());
                }
            }
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Windows + everything else: we don't currently surface a
        // machine-id. The hashed-empty default keeps the schema
        // contract intact ("hash is always present").
        None
    }
}

/// Sysinfo's `cpus()` may return zero on a stripped container —
/// fall back to `num_cpus`-style logic via the standard library.
fn num_cpus_fallback() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn hash16(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut s = String::with_capacity(16);
    use std::fmt::Write;
    for b in digest.iter().take(8) {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_returns_non_empty_fields() {
        let h = HostInfo::collect();
        assert!(!h.os.is_empty());
        assert!(!h.arch.is_empty());
        assert!(h.cpu_count > 0);
        assert!(h.total_memory_mb > 0);
        assert_eq!(h.hostname_hash.len(), 16);
        assert_eq!(h.machine_id_hash.len(), 16);
    }

    #[test]
    fn hash16_is_deterministic_and_truncated() {
        assert_eq!(hash16("foo"), hash16("foo"));
        assert_ne!(hash16("foo"), hash16("bar"));
        assert_eq!(hash16("anything").len(), 16);
        assert!(hash16("anything").chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn empty_input_produces_well_known_hash() {
        // sha256("")[..8] = e3b0c44298fc1c14
        assert_eq!(hash16(""), "e3b0c44298fc1c14");
    }
}
