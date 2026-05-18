//! `crabcc setup install-integrations` — wire crabcc into Cursor, Gemini,
//! OpenCode, LangChain/LangGraph, OS-native services, and kernel builds.
//!
//! Deliberately does not overwrite global agent settings without `--yes`.
//! Project-level MCP merges are opt-in via `--project`.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const MCP_CRABCC: &str = include_str!("../../../install/integrations/mcp-crabcc.json");
const HOOKS_CURSOR: &str = include_str!("../../../install/integrations/hooks-cursor.json");
const GEMINI_FRAGMENT: &str = include_str!("../../../install/integrations/gemini-settings.fragment.json");
const OPENCODE_FRAGMENT: &str = include_str!("../../../install/integrations/opencode.fragment.jsonc");
const HOOK_HINT_SH: &str = include_str!("../../../install/integrations/hooks/crabcc-hint.sh");
const OS_SERVICE: &str = include_str!("../../../install/integrations/os/crabcc-mcp.service");
const OS_PLIST: &str = include_str!("../../../install/integrations/os/com.crabcc.mcp.plist");
const SKILL_MD: &str = include_str!("../../../skill/crabcc/SKILL.md");

const LANGCHAIN_EMBEDDED: &[(&str, &str)] = &[
    (
        "pyproject.toml",
        include_str!("../../../install/integrations/langchain/pyproject.toml"),
    ),
    (
        "README.md",
        include_str!("../../../install/integrations/langchain/README.md"),
    ),
    (
        "crabcc_langchain/__init__.py",
        include_str!("../../../install/integrations/langchain/crabcc_langchain/__init__.py"),
    ),
    (
        "crabcc_langchain/tools.py",
        include_str!("../../../install/integrations/langchain/crabcc_langchain/tools.py"),
    ),
    (
        "crabcc_langchain/graph.py",
        include_str!("../../../install/integrations/langchain/crabcc_langchain/graph.py"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Cursor,
    Claude,
    Gemini,
    Opencode,
    Langchain,
    Os,
    Kernel,
}

impl Target {
    fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cursor" => Ok(Self::Cursor),
            "claude" => Ok(Self::Claude),
            "gemini" => Ok(Self::Gemini),
            "opencode" => Ok(Self::Opencode),
            "langchain" | "langgraph" => Ok(Self::Langchain),
            "os" | "native" => Ok(Self::Os),
            "kernel" => Ok(Self::Kernel),
            "all" => bail!("use expand_targets instead of parsing 'all'"),
            other => bail!(
                "unknown target '{other}' — expected cursor|claude|gemini|opencode|langchain|os|kernel|all"
            ),
        }
    }

    fn all() -> Vec<Self> {
        vec![
            Self::Cursor,
            Self::Claude,
            Self::Gemini,
            Self::Opencode,
            Self::Langchain,
            Self::Os,
            Self::Kernel,
        ]
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
            Target::Cursor => install_cursor(opts, project_root)?,
            Target::Claude => install_claude(opts)?,
            Target::Gemini => install_gemini(opts)?,
            Target::Opencode => install_opencode(opts)?,
            Target::Langchain => install_langchain(opts)?,
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
        Target::Cursor => "cursor",
        Target::Claude => "claude",
        Target::Gemini => "gemini",
        Target::Opencode => "opencode",
        Target::Langchain => "langchain",
        Target::Os => "os",
        Target::Kernel => "kernel",
    }
}

fn install_cursor(opts: Options, project_root: &Path) -> Result<()> {
    let home = home_dir().ok_or_else(|| anyhow!("$HOME not set"))?;
    let global_skill = home.join(".cursor/skills/crabcc/SKILL.md");

    install_skill(&global_skill, opts)?;

    if opts.project {
        let proj_skill = project_root.join(".cursor/skills/crabcc/SKILL.md");
        install_skill(&proj_skill, opts)?;

        let mcp_path = project_root.join(".mcp.json");
        merge_mcp_file(&mcp_path, opts)?;

        let hooks_dir = project_root.join(".cursor/hooks");
        install_hook_script(&hooks_dir.join("crabcc-hint.sh"), opts)?;
        println!(
            "  hooks: merge {} into .cursor/hooks.json (or use install/integrations/hooks-cursor.json)",
            "hooks-cursor"
        );
        if opts.dry_run {
            println!("  [dry-run] would print hooks-cursor.json");
        } else {
            println!("{HOOKS_CURSOR}");
        }
    }

    println!();
    println!("Global MCP (~/.cursor/mcp.json) — merge:");
    println!("{MCP_CRABCC}");
    println!();
    println!("Restart Cursor → Settings → MCP → enable `crabcc`.");
    Ok(())
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

fn install_gemini(opts: Options) -> Result<()> {
    println!("Merge into ~/.gemini/settings.json (user) and/or .gemini/settings.json (project):");
    println!("{GEMINI_FRAGMENT}");
    if !opts.dry_run {
        let dest = integrations_home()?.join("gemini-settings.fragment.json");
        write_atomic(&dest, GEMINI_FRAGMENT, opts.yes)?;
        println!("  also wrote: {}", dest.display());
    }
    Ok(())
}

fn install_opencode(opts: Options) -> Result<()> {
    println!("Merge into ~/.config/opencode/opencode.json and/or project opencode.json:");
    println!("{OPENCODE_FRAGMENT}");
    super::install::ensure_opencode(opts.yes)?;
    if !opts.dry_run {
        let dest = integrations_home()?.join("opencode.fragment.jsonc");
        write_atomic(&dest, OPENCODE_FRAGMENT, opts.yes)?;
        println!("  also wrote: {}", dest.display());
    }
    Ok(())
}

fn install_langchain(opts: Options) -> Result<()> {
    let dest = integrations_home()?.join("langchain");
    if opts.dry_run {
        println!("  [dry-run] would materialize {}", dest.display());
        return Ok(());
    }

    if let Ok(repo) = git_repo_root() {
        let src = repo.join("install/integrations/langchain");
        if src.is_dir() {
            symlink_dir(&src, &dest, opts.yes)?;
            println!("  linked: {} -> {}", dest.display(), src.display());
            println!("  next: cd {} && pip install -e .", dest.display());
            return Ok(());
        }
    }

    for (rel, content) in LANGCHAIN_EMBEDDED {
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomic(&path, content, true)?;
    }
    println!("  wrote embedded langchain package to {}", dest.display());
    println!("  next: cd {} && pip install -e .", dest.display());
    println!("  LangSmith: export LANGSMITH_API_KEY + LANGCHAIN_TRACING_V2=true");
    println!("  batch eval: tools/orchestrator/import-dataset.sh");
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
    println!("  wrote: {}/{{crabcc-mcp.service,com.crabcc.mcp.plist,README.md}}", dest.display());
    println!("  macOS: cp {} ~/Library/LaunchAgents/ && launchctl load …", dest.join("com.crabcc.mcp.plist").display());
    println!("  linux: cp {} ~/.config/systemd/user/ && systemctl --user enable crabcc-mcp", dest.join("crabcc-mcp.service").display());
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

fn merge_mcp_file(path: &Path, opts: Options) -> Result<()> {
    let merged = merge_mcp_json(path, MCP_CRABCC)?;
    if opts.dry_run {
        println!("  [dry-run] would write {}", path.display());
        println!("{merged}");
        return Ok(());
    }
    if path.exists() && !opts.yes && !confirm(&format!("overwrite/merge {}? [y/N] ", path.display()))? {
        println!("  skipped {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(path, &merged, true)?;
    println!("  ✓ merged MCP into {}", path.display());
    Ok(())
}

fn merge_mcp_json(existing_path: &Path, fragment: &str) -> Result<String> {
    let frag: serde_json::Value = serde_json::from_str(fragment).context("mcp fragment JSON")?;
    let frag_servers = frag
        .get("mcpServers")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let mut base = if existing_path.exists() {
        let s = std::fs::read_to_string(existing_path)?;
        serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({ "mcpServers": {} }))
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

fn install_hook_script(path: &Path, opts: Options) -> Result<()> {
    if opts.dry_run {
        println!("  [dry-run] would write {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(path, HOOK_HINT_SH, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    println!("  ✓ hook script {}", path.display());
    Ok(())
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
        bail!("not inside a git repository");
    }
    let s = String::from_utf8(out.stdout)?.trim().to_string();
    if s.is_empty() {
        bail!("empty git root");
    }
    Ok(PathBuf::from(s))
}

fn write_atomic(path: &Path, content: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
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

fn symlink_dir(src: &Path, dst: &Path, yes: bool) -> Result<()> {
    if dst.exists() {
        if let Ok(link) = std::fs::read_link(dst) {
            if link == src {
                return Ok(());
            }
        }
        if !yes {
            bail!("{} exists — pass --yes to replace", dst.display());
        }
        std::fs::remove_dir_all(dst).ok();
        let _ = std::fs::remove_file(dst);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, dst)?;
    #[cfg(not(unix))]
    bail!("symlinks require Unix");
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
}
