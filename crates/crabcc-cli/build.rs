// Capture git + build metadata at compile time. Emits these env vars for
// `env!()` reads from main.rs:
//
//   CRABCC_BUILD_COMMIT  — `git rev-parse --short=12 HEAD` (or "unknown")
//   CRABCC_BUILD_BRANCH  — `git rev-parse --abbrev-ref HEAD` (or "unknown")
//   CRABCC_BUILD_TAG     — `git describe --tags --exact-match HEAD` if HEAD is
//                          tagged, otherwise empty string
//   CRABCC_BUILD_TIME    — UTC ISO-8601 (date -u +%Y-%m-%dT%H:%M:%SZ)
//   CRABCC_BUILD_TARGET  — Cargo's TARGET triple
//
// Robust against shallow / detached / no-git checkouts (release runners
// sometimes are): all git failures fall back to "unknown" or "" so the
// build never breaks. Also re-runs whenever .git/HEAD changes, so dev
// rebuilds reflect the current commit instead of the first one cargo saw.

use std::process::Command;

fn main() {
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let tag = git(&["describe", "--tags", "--exact-match", "HEAD"]).unwrap_or_default();
    let time = utc_iso8601();
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".into());

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());

    println!("cargo:rustc-env=CRABCC_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=CRABCC_BUILD_BRANCH={branch}");
    println!("cargo:rustc-env=CRABCC_BUILD_TAG={tag}");
    println!("cargo:rustc-env=CRABCC_BUILD_TIME={time}");
    println!("cargo:rustc-env=CRABCC_BUILD_TARGET={target}");
    println!("cargo:rustc-env=CRABCC_BUILD_PROFILE={profile}");

    // Re-run on commit changes so dev rebuilds pick up the latest sha
    // without needing a `cargo clean`. The `.git/HEAD` watch covers both
    // commit-on-branch and branch-switch.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn utc_iso8601() -> String {
    // Avoid an extra dep; shell out to `date -u`. Works on macOS + Linux.
    // If date is missing for some unfathomable reason, fall back to "unknown".
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".into(),
    }
}
