//! `crabcc setup install-integrations` — wire crabcc into Claude Code,
//! pi, OS-native services, and kernel builds.
//!
//! v4.5 narrowed the integration surface to two agents: Claude Code (the
//! "big" example) and pi (the "tiny" example). Cursor / Gemini / OpenCode /
//! LangChain were removed as part of the sharpening release.
//!
//! Deliberately does not overwrite global agent settings without `--yes`.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const OS_SERVICE: &str = include_str!("../../../install/integrations/os/crabcc-mcp.service");
const OS_PLIST: &str = include_str!("../../../install/integrations/os/com.crabcc.mcp.plist");
const SKILL_MD: &str = include_str!("../../../skill/crabcc/SKILL.md");
const PI_FRAGMENT: &str = include_str!("../../../install/integrations/pi.fragment.json");
#[allow(dead_code)] // referenced by tests; kept embedded so the snippet ships with the binary
const MCP_CRABCC: &str = include_str!("../../../install/integrations/mcp-crabcc.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Claude,
    Pi,
    Os,
    Kernel,
}

impl Target {
    fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "pi" => Ok(Self::Pi),
            "os" | "native" => Ok(Self::Os),
            "kernel" => Ok(Self::Kernel),
            "all" => bail!("use expand_targets instead of parsing 'all'"),
            other => bail!("unknown target '{other}' — expected claude|pi|os|kernel|all"),
        }
    }

    fn all() -> Vec<Self> {
        vec![Self::Claude, Self::Pi, Self::Os, Self::Kernel]
    }
}

pub fn expand_targets(names: &[String]) -> Result<Vec<Target>> {
    if names.is_empty() {
        return Ok(Target::all());
    }
    let mut set = HashSet::new();
    for n in names {
        if n.eq_ignore_ascii_case("all") {
            return Ok(Target::all());
        }
        set.insert(Target::parse(n)?);
    }
    Ok(set.into_iter().collect())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub yes: bool,
    #[allow(dead_code)] // reserved for project-level merges; no targets need it post-v4.5
    pub project: bool,
    pub dry_run: bool,
}

pub fn run(targets: &[Target], opts: Options, project_root: &Path) -> Result<()> {
    println!("crabcc install-integrations");
    if opts.dry_run {
        println!("(dry-run — no files will be touched)\n");
    }
    for t in targets {
        println!("── target: {} ──", target_label(*t));
        match t {
            Target::Claude => install_claude(opts)?,
            Target::Pi => install_pi(opts, project_root)?,
            Target::Os => install_os(opts)?,
            Target::Kernel => install_kernel(opts)?,
        }
        println!();
    }
    println!("Done. Full guide: install/integrations.md (or AGENTS.md § Integrations).");
    Ok(())
}

fn target_label(t: Target) -> &'static str {
    match t {
        Target::Claude => "claude",
        Target::Pi => "pi",
        Target::Os => "os",
        Target::Kernel => "kernel",
    }
}

fn install_claude(opts: Options) -> Result<()> {
    if opts.dry_run {
        println!("  [dry-run] would run install-claude");
        return Ok(());
    }
    super::install::run(super::install::InstallOptions {
        yes: opts.yes,
        print_hooks_only: false,
        with_ollama_stack: false,
        print_stack_instructions: false,
        dry_run: false,
    })
}

/// Install crabcc as a pi agent skill.
///
/// pi reads skills from `~/.pi/agent/skills/<name>/SKILL.md` (global) and
/// `.pi/skills/<name>/SKILL.md` (project) and enables them via the `skills`
/// array in `~/.pi/agent/settings.json` or `.pi/settings.json`.
/// See https://pi.dev/docs/latest/settings — pi uses skills + extensions
/// rather than MCP servers.
fn install_pi(opts: Options, project_root: &Path) -> Result<()> {
    let home = home_dir().ok_or_else(|| anyhow!("$HOME not set"))?;
    let global_skill = home.join(".pi/agent/skills/crabcc/SKILL.md");
    install_skill(&global_skill, opts)?;

    if opts.project {
        let proj_skill = project_root.join(".pi/skills/crabcc/SKILL.md");
        install_skill(&proj_skill, opts)?;
    }

    println!();
    println!(
        "Merge into ~/.pi/agent/settings.json (global) or .pi/settings.json (project):"
    );
    println!("{PI_FRAGMENT}");
    if !opts.dry_run {
        let dest = integrations_home()?.join("pi.fragment.json");
        write_atomic(&dest, PI_FRAGMENT, opts.yes)?;
        println!("  also wrote: {}", dest.display());
    }
    Ok(())
}

fn install_skill(dst: &Path, opts: Options) -> Result<()> {
    if let Some(parent) = dst.parent() {
        if opts.dry_run {
            println!("  [dry-run] would mkdir {}", parent.display());
        } else {
            std::fs::create_dir_all(parent)?;
        }
    }
    if let Ok(repo) = git_repo_root() {
        let src = repo.join("skill/crabcc/SKILL.md");
        if src.exists() {
            symlink_file(&src, dst, opts)?;
            return Ok(());
        }
    }
    write_atomic(dst, SKILL_MD, opts.yes)
}

fn symlink_file(src: &Path, dst: &Path, opts: Options) -> Result<()> {
    let action = format!("symlink {} -> {}", dst.display(), src.display());
    if opts.dry_run {
        println!("  [dry-run] would {action}");
        return Ok(());
    }
    if dst.exists() {
        if let Ok(link) = std::fs::read_link(dst) {
            if link == src {
                println!("  ✓ already linked: {}", dst.display());
                return Ok(());
            }
        }
    }
    if !opts.yes && !confirm(&format!("{action}? [y/N] "))? {
        println!("  skipped");
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(dst);
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, dst).with_context(|| action.clone())?;
    #[cfg(not(unix))]
    bail!("symlinks require Unix");
    println!("  ✓ {action}");
    Ok(())
}

fn install_os(opts: Options) -> Result<()> {
    let home = home_dir().ok_or_else(|| anyhow!("$HOME not set"))?;
    let dest = integrations_home()?.join("os");
    if opts.dry_run {
        println!("  [dry-run] would write OS templates to {}", dest.display());
        return Ok(());
    }
    std::fs::create_dir_all(&dest)?;
    let home_s = home.to_string_lossy();
    let plist = OS_PLIST.replace("__HOME__", &home_s);
    write_atomic(&dest.join("crabcc-mcp.service"), OS_SERVICE, true)?;
    write_atomic(&dest.join("com.crabcc.mcp.plist"), &plist, true)?;
    write_atomic(
        &dest.join("README.md"),
        include_str!("../../../install/integrations/os/README.md"),
        true,
    )?;
    println!(
        "  wrote: {}/{{crabcc-mcp.service,com.crabcc.mcp.plist,README.md}}",
        dest.display()
    );
    println!(
        "  macOS: cp {} ~/Library/LaunchAgents/ && launchctl load …",
        dest.join("com.crabcc.mcp.plist").display()
    );
    println!(
        "  linux: cp {} ~/.config/systemd/user/ && systemctl --user enable crabcc-mcp",
        dest.join("crabcc-mcp.service").display()
    );
    Ok(())
}

fn install_kernel(opts: Options) -> Result<()> {
    println!("Stable kernel (6.6 LTS, Apple Containers):");
    println!("  install/kernel/build.sh");
    println!();
    println!("Bleeding-edge (6.12+, io_uring + BPF + btrfs):");
    println!(
        "  LINUX_VERSION=6.12.20 install/kernel/build.sh '' install/kernel/config.bleeding-edge.fragment"
    );
    if let Ok(repo) = git_repo_root() {
        let readme = repo.join("install/kernel/README.md");
        if readme.exists() && !opts.dry_run {
            println!("  docs: {}", readme.display());
        }
    }
    Ok(())
}

#[allow(dead_code)] // referenced by tests; preserved so a future integration can reuse it
fn merge_mcp_json(existing_path: &Path, fragment: &str) -> Result<String> {
    let frag: serde_json::Value = serde_json::from_str(fragment).context("mcp fragment JSON")?;
    let frag_servers = frag
        .get("mcpServers")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let mut base = if existing_path.exists() {
        let s = std::fs::read_to_string(existing_path)?;
        match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "warning: corrupt {} ({e}); starting fresh",
                    existing_path.display()
                );
                serde_json::json!({ "mcpServers": {} })
            }
        }
    } else {
        serde_json::json!({ "mcpServers": {} })
    };

    let obj = base
        .as_object_mut()
        .ok_or_else(|| anyhow!("MCP config root must be a JSON object"))?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(dst) = servers.as_object_mut() {
        if let Some(src) = frag_servers.as_object() {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(serde_json::to_string_pretty(&base)?)
}

fn integrations_home() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow!("$HOME not set"))?;
    let p = home.join(".crabcc/integrations");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

fn home_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    None
}

fn git_repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to invoke git")?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            bail!("not inside a git repository");
        }
        bail!("not inside a git repository: {msg}");
    }
    let s = String::from_utf8(out.stdout)?.trim().to_string();
    if s.is_empty() {
        bail!("empty git root");
    }
    Ok(PathBuf::from(s))
}

fn write_atomic(path: &Path, content: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        println!("  skipped (exists): {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("crabcc.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename -> {}", path.display()))?;
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_mcp_adds_crabcc_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        let out = merge_mcp_json(&path, MCP_CRABCC).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcpServers"]["crabcc"].is_object());
        assert!(v["mcpServers"]["other"].is_object());
    }

    #[test]
    fn expand_all_targets() {
        let t = expand_targets(&["all".to_string()]).unwrap();
        assert_eq!(t.len(), Target::all().len());
    }

    #[test]
    fn write_atomic_skips_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.md");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, "new", false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }

    #[test]
    fn write_atomic_force_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.md");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, "new", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn write_atomic_creates_nested_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/skill.md");
        write_atomic(&path, SKILL_MD, true).unwrap();
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path).unwrap().contains("crabcc"));
    }

    #[test]
    fn merge_mcp_json_recovers_from_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, "{not json").unwrap();
        let out = merge_mcp_json(&path, MCP_CRABCC).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["mcpServers"]["crabcc"].is_object());
    }

    #[test]
    fn write_atomic_embedded_skill_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cursor/skills/crabcc/SKILL.md");
        write_atomic(&path, SKILL_MD, true).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("crabcc"));
    }
}
