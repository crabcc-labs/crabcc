//! `crabcc install-claude` — symlink skill + slash-command into `~/.claude/`,
//! then PRINT (never write) the `claude mcp add` invocation and hook JSON
//! snippets for the user to paste into `~/.claude/settings.json`.
//!
//! Deliberately does NOT touch `~/.claude.json` or `~/.claude/settings.json`.
//! The user wants surprise-free installs.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Embedded hook template — single source of truth lives at
/// `install/hooks-claude.json` at the repo root, baked into the binary so
/// `crabcc install-claude` works after `cargo install` (no repo needed).
const HOOKS_JSON: &str = include_str!("../../../install/hooks-claude.json");

pub fn run(yes: bool, print_hooks_only: bool) -> Result<()> {
    if print_hooks_only {
        // Round-trip through serde_json so output is canonicalised pretty JSON.
        let v: serde_json::Value = serde_json::from_str(HOOKS_JSON)
            .context("embedded hooks-claude.json is not valid JSON")?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    let repo_root = git_repo_root()?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("$HOME is not set"))?;

    let pairs: &[(PathBuf, PathBuf)] = &[
        (
            repo_root.join("skill/crabcc/SKILL.md"),
            home.join(".claude/skills/crabcc/SKILL.md"),
        ),
        (
            repo_root.join("commands/crabcc-init.md"),
            home.join(".claude/commands/crabcc-init.md"),
        ),
        (
            repo_root.join("commands/crabcc-upgrade.md"),
            home.join(".claude/commands/crabcc-upgrade.md"),
        ),
        (
            repo_root.join("commands/crabcc-install.md"),
            home.join(".claude/commands/crabcc-install.md"),
        ),
    ];

    for (src, dst) in pairs {
        if !src.exists() {
            bail!("source missing: {}", src.display());
        }
        link_one(src, dst, yes)?;
    }

    println!();
    println!("Next steps (run yourself):");
    println!();
    println!("  claude mcp add crabcc -- crabcc --mcp");
    println!();
    println!("Optional Claude Code hooks (copy into ~/.claude/settings.json under \"hooks\"):");
    println!();
    let v: serde_json::Value = serde_json::from_str(HOOKS_JSON)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    println!();
    println!("Then in Claude Code: /reload-plugins");

    Ok(())
}

fn git_repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to invoke `git` — is it on PATH?")?;
    if !out.status.success() {
        bail!("not inside a git repository (run `crabcc install-claude` from the crabcc checkout)");
    }
    let s = String::from_utf8(out.stdout)?.trim().to_string();
    if s.is_empty() {
        bail!("`git rev-parse --show-toplevel` returned empty output");
    }
    Ok(PathBuf::from(s))
}

fn link_one(src: &Path, dst: &Path, yes: bool) -> Result<()> {
    // Idempotent: if dst is already a symlink to src, skip.
    if let Ok(existing) = std::fs::read_link(dst) {
        if existing == src {
            println!("already linked: {} -> {}", dst.display(), src.display());
            return Ok(());
        }
    }

    if !yes
        && !confirm(&format!(
            "Symlink {} -> {}? [y/N] ",
            src.display(),
            dst.display()
        ))?
    {
        println!("  skipped");
        return Ok(());
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }

    // Replicate `ln -sf` — clobber any existing file/symlink.
    if dst.exists() || std::fs::symlink_metadata(dst).is_ok() {
        std::fs::remove_file(dst).ok();
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(src, dst)
        .with_context(|| format!("symlink {} -> {}", dst.display(), src.display()))?;

    #[cfg(not(unix))]
    bail!("install-claude only supports Unix (uses symlinks)");

    println!("  linked: {} -> {}", dst.display(), src.display());
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}
