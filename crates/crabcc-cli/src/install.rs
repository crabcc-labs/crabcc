//! `crabcc install-claude` — install crabcc's skill + slash commands into the
//! user's Claude Code config dir, then PRINT the `claude mcp add` invocation
//! and hook JSON snippets for the user to paste into their `settings.json`.
//!
//! Deliberately does NOT touch `settings.json` itself. The user wants
//! surprise-free installs.
//!
//! # Config-dir resolution
//!
//! Honors `$CLAUDE_CONFIG_DIR` if set (this is Claude Code's documented
//! override). Falls back to `$HOME/.claude`. Same precedence Claude Code
//! itself uses, so installs land where Claude Code looks.
//!
//! # Repo-or-binary
//!
//! When run from inside the crabcc git checkout, links to the live source
//! files (so `git pull` propagates). When run after `cargo install crabcc-cli`
//! from anywhere else, writes embedded copies of the skill + commands. Either
//! way `install-claude` succeeds.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Embedded hook template — single source of truth lives at
/// `install/hooks-claude.json` at the repo root, baked into the binary so
/// `crabcc install-claude` works after `cargo install` (no repo needed).
const HOOKS_JSON: &str = include_str!("../../../install/hooks-claude.json");

/// Embedded copies of the skill + slash commands. Written to the Claude
/// config dir when no live repo is reachable; in a live checkout we still
/// prefer symlinking to the on-disk source so edits propagate without a
/// rebuild. Path is the destination basename relative to `<config>/`.
const EMBEDDED_ASSETS: &[(&str, &str)] = &[
    (
        "skills/crabcc/SKILL.md",
        include_str!("../../../skill/crabcc/SKILL.md"),
    ),
    (
        "commands/crabcc-init.md",
        include_str!("../../../commands/crabcc-init.md"),
    ),
    (
        "commands/crabcc-upgrade.md",
        include_str!("../../../commands/crabcc-upgrade.md"),
    ),
    (
        "commands/crabcc-install.md",
        include_str!("../../../commands/crabcc-install.md"),
    ),
    (
        "commands/crabcc/generate/context.md",
        include_str!("../../../commands/crabcc/generate/context.md"),
    ),
];

/// Embedded Ollama auth stack — issue #105 Phase 5b. Same compile-time
/// embedding pattern as HOOKS_JSON so `crabcc install-claude
/// --with-ollama-stack` works after `cargo install` without a checkout.
/// Materializes to `~/.crabcc/ollama-stack/` on demand. (mode, bytes)
const OLLAMA_STACK_FILES: &[(&str, u32, &[u8])] = &[
    (
        "docker-compose.yml",
        0o644,
        include_bytes!("../../../install/ollama-stack/docker-compose.yml"),
    ),
    (
        "Caddyfile",
        0o644,
        include_bytes!("../../../install/ollama-stack/Caddyfile"),
    ),
    (
        "litellm.config.yaml",
        0o644,
        include_bytes!("../../../install/ollama-stack/litellm.config.yaml"),
    ),
    (
        ".env.example",
        0o644,
        include_bytes!("../../../install/ollama-stack/.env.example"),
    ),
    (
        "init-keys.sh",
        0o755,
        include_bytes!("../../../install/ollama-stack/init-keys.sh"),
    ),
    (
        "README.md",
        0o644,
        include_bytes!("../../../install/ollama-stack/README.md"),
    ),
    (
        "MANUAL_TEST_CHECKLIST.md",
        0o644,
        include_bytes!("../../../install/ollama-stack/MANUAL_TEST_CHECKLIST.md"),
    ),
];

/// Caller-provided flags for `crabcc install-claude`.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    pub yes: bool,
    pub print_hooks_only: bool,
    pub with_ollama_stack: bool,
    pub print_stack_instructions: bool,
    /// `--dry-run` — print the planned operations without touching disk.
    pub dry_run: bool,
}

/// Resolve the Claude Code config directory.
///
/// Precedence:
///   1. `$CLAUDE_CONFIG_DIR` if set and non-empty (Claude Code's documented
///      override).
///   2. `$HOME/.claude` (the documented default on macOS / Linux).
///
/// Returns the resolved path even if it doesn't yet exist — callers create
/// subdirectories as needed via `link_or_write` below.
pub fn claude_config_dir() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let s = v.to_string_lossy();
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    home_dir()
        .map(|h| h.join(".claude"))
        .ok_or_else(|| anyhow!("$HOME is not set and $CLAUDE_CONFIG_DIR is empty"))
}

/// Cross-shell home-directory lookup. `$HOME` on Unix; `%USERPROFILE%` on
/// Windows; falls back to `$HOMEDRIVE\$HOMEPATH` if needed. Returns `None`
/// only when none of the above are set — extremely rare outside containers.
fn home_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
            (Some(d), Some(p)) if !d.is_empty() && !p.is_empty() => {
                let mut s = std::ffi::OsString::from(d);
                s.push(p);
                return Some(PathBuf::from(s));
            }
            _ => {}
        }
    }
    None
}

pub fn run(opts: InstallOptions) -> Result<()> {
    if opts.print_hooks_only {
        let v: serde_json::Value = serde_json::from_str(HOOKS_JSON)
            .context("embedded hooks-claude.json is not valid JSON")?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    if opts.print_stack_instructions {
        return print_stack_instructions();
    }

    let yes = opts.yes;
    let dry = opts.dry_run;

    let config_dir = claude_config_dir()?;
    let config_dir_source = if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
        "CLAUDE_CONFIG_DIR"
    } else {
        "$HOME/.claude default"
    };

    println!(
        "Claude config dir: {} ({})",
        config_dir.display(),
        config_dir_source
    );
    if dry {
        println!("(dry-run — no files will be touched)");
    }
    println!();

    // Try to find a live source repo for symlinking. If we're not in one,
    // every asset falls back to writing the embedded copy — so install
    // still succeeds after `cargo install crabcc-cli` from any cwd.
    let repo_root = git_repo_root().ok();
    if let Some(ref r) = repo_root {
        println!("Source repo: {} (will symlink, live updates)", r.display());
    } else {
        println!("Source repo: none reachable (will write embedded copies)");
    }
    println!();

    for (rel, embedded) in EMBEDDED_ASSETS {
        let dst = config_dir.join(rel);
        let live_src = repo_root
            .as_ref()
            .map(|r| r.join(asset_repo_path(rel)))
            .filter(|p| p.exists());
        link_or_write(live_src.as_deref(), embedded, &dst, yes, dry)?;
    }

    // RTK is a runtime dependency for the hook-based shell-output proxy
    // that keeps token use lean inside Claude Code. We don't bake it in —
    // `cargo install rtk` keeps it upgradable on its own cadence — but
    // bootstrap is the right place to check + prompt.
    if !dry {
        ensure_rtk(yes)?;
        ensure_opencode(yes)?;
        ensure_bash5(yes)?;
    }

    println!();
    println!("Next steps (run yourself):");
    println!();
    println!("  claude mcp add crabcc -- crabcc --mcp");
    println!();
    println!(
        "Optional Claude Code hooks (copy into {}/settings.json under \"hooks\"):",
        config_dir.display()
    );
    println!();
    let v: serde_json::Value = serde_json::from_str(HOOKS_JSON)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    println!();
    println!("Then in Claude Code: /reload-plugins");

    if opts.with_ollama_stack {
        if dry {
            println!();
            println!("(dry-run — would materialize ollama-stack to ~/.crabcc/ollama-stack/)");
        } else {
            // Ollama stack is OS-local user state, not Claude config — keep
            // it under $HOME/.crabcc/ regardless of $CLAUDE_CONFIG_DIR.
            let home =
                home_dir().ok_or_else(|| anyhow!("$HOME unset; cannot place ollama stack"))?;
            materialize_ollama_stack(&home)?;
            println!();
            println!("Ollama stack materialized; bringing it up...");
            crabcc_core::ollama_stack::check_docker()?;
            let ols_opts = crabcc_core::ollama_stack::Options::new()
                .with_compose_dir(home.join(".crabcc/ollama-stack"));
            let up = crabcc_core::ollama_stack::up(&ols_opts).context(
                "compose up failed; check docker compose logs and re-run \
                 `crabcc install-claude --with-ollama-stack`. \
                 Run `crabcc doctor` for a full environment audit.",
            )?;
            println!(
                "  stack ready: {} services healthy in {} ms",
                up.services_healthy.len(),
                up.duration_ms
            );
        }
    }

    Ok(())
}

/// Map a destination relative path (`commands/crabcc-init.md`) to the
/// matching repo-relative source path. Mostly identity — the skill lives
/// at `skill/crabcc/SKILL.md` in the repo (singular `skill/`) but at
/// `skills/crabcc/SKILL.md` in the Claude config dir (plural `skills/`).
fn asset_repo_path(dst_rel: &str) -> PathBuf {
    if let Some(rest) = dst_rel.strip_prefix("skills/") {
        PathBuf::from("skill").join(rest)
    } else {
        PathBuf::from(dst_rel)
    }
}

/// Phase 5b — write the embedded `install/ollama-stack/` files to
/// `$HOME/.crabcc/ollama-stack/` so the user has a writable, OS-local
/// copy of the Compose recipe. Idempotent — overwrites on each call so
/// upgrading crabcc picks up Caddyfile / docker-compose.yml changes.
/// Preserves any existing `.env` (real secrets) by skipping it when
/// already on disk.
fn materialize_ollama_stack(home: &Path) -> Result<()> {
    let dest = home.join(".crabcc/ollama-stack");
    std::fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;

    for (name, mode, bytes) in OLLAMA_STACK_FILES {
        let path = dest.join(name);
        std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perm = std::fs::Permissions::from_mode(*mode);
            let _ = std::fs::set_permissions(&path, perm);
        }
        println!("wrote: {}", path.display());
    }

    let env_path = dest.join(".env");
    if !env_path.exists() {
        eprintln!(
            "  hint: no .env yet at {} — run `{}/init-keys.sh` to generate keys",
            env_path.display(),
            dest.display()
        );
    }
    Ok(())
}

fn print_stack_instructions() -> Result<()> {
    println!("# crabcc Ollama auth stack — manual bring-up (issue #105)");
    println!();
    println!("# 1. Materialize embedded files (or use the repo copy at install/ollama-stack/)");
    println!("crabcc install-claude --with-ollama-stack");
    println!();
    println!("# 2. Bootstrap shared Docker network");
    println!("install/init-shared-network.sh");
    println!();
    println!("# 3. Generate API keys (writes ~/.crabcc/ollama-stack/.env, mode 600)");
    println!("~/.crabcc/ollama-stack/init-keys.sh");
    println!();
    println!("# 4. Bring stack up");
    println!("crabcc ollama-stack up   # or:  ccc setup --ollama-up");
    println!();
    println!("# 5. Smoke");
    println!("crabcc ollama-stack status");
    println!("crabcc agent --backend ollama --run \"ping\" --dry-run");
    Ok(())
}

/// Try to find the crabcc git repo root. Soft-fails (returns Err) when the
/// caller is not inside a git checkout — the caller falls back to embedded
/// assets in that case.
fn git_repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to invoke `git`")?;
    if !out.status.success() {
        bail!("not inside a git repository");
    }
    let s = String::from_utf8(out.stdout)?.trim().to_string();
    if s.is_empty() {
        bail!("`git rev-parse --show-toplevel` returned empty output");
    }
    Ok(PathBuf::from(s))
}

/// Either symlink to a live source file or write an embedded copy.
///
/// - `live_src`: Some(path) when a checkout is reachable AND the path exists.
///   The function symlinks dst → live_src so subsequent `git pull` updates
///   propagate without re-installing.
/// - `live_src` = None: writes `embedded` to dst as a regular file.
///
/// Both paths are atomic w.r.t. the target: symlink uses rename-into-place,
/// regular files use a tmp-file + rename, so an interrupted install never
/// leaves the target missing while it's being recreated.
fn link_or_write(
    live_src: Option<&Path>,
    embedded: &str,
    dst: &Path,
    yes: bool,
    dry: bool,
) -> Result<()> {
    // Idempotent fast path: if already linked to the requested source, skip.
    if let Some(src) = live_src {
        if let Ok(existing) = std::fs::read_link(dst) {
            if existing == src {
                println!("  ✓ already linked: {}", dst.display());
                return Ok(());
            }
        }
    }

    let action = if let Some(src) = live_src {
        format!("symlink {} -> {}", dst.display(), src.display())
    } else {
        format!("write embedded copy to {}", dst.display())
    };

    if dry {
        println!("  [dry-run] would {}", action);
        return Ok(());
    }

    if !yes && !confirm(&format!("{}? [y/N] ", action))? {
        println!("    skipped");
        return Ok(());
    }

    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", dst.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;

    // Atomic install via temp-then-rename. Avoids the race window where
    // dst is removed before its replacement is in place.
    let tmp = parent.join(format!(
        ".{}.crabcc-install.tmp",
        dst.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "asset".to_string())
    ));
    let _ = std::fs::remove_file(&tmp);

    if let Some(src) = live_src {
        #[cfg(unix)]
        std::os::unix::fs::symlink(src, &tmp)
            .with_context(|| format!("symlink {} -> {}", tmp.display(), src.display()))?;
        #[cfg(not(unix))]
        bail!("install-claude only supports symlinks on Unix");
    } else {
        std::fs::write(&tmp, embedded).with_context(|| format!("write {}", tmp.display()))?;
    }

    std::fs::rename(&tmp, dst).with_context(|| {
        format!(
            "rename {} -> {} (target may be on a different filesystem)",
            tmp.display(),
            dst.display()
        )
    })?;

    println!("  ✓ {}", action);
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Result of probing PATH for `rtk`. The name collides between the
/// wanted "Rust Token Killer" (rtk gain / rtk discover / rtk proxy)
/// and reachingforthejack/rtk ("Rust Type Kit"); the latter has no
/// `gain` subcommand, so we use that as the discriminator. The user's
/// global RTK.md (loaded into every Claude Code session) calls out
/// the same collision — keep this aligned with it.
#[derive(Debug, PartialEq, Eq)]
enum RtkStatus {
    Token,
    Wrong,
    Missing,
}

fn detect_rtk() -> RtkStatus {
    let probe = Command::new("rtk").arg("gain").arg("--help").output();
    match probe {
        Ok(out) if out.status.success() => RtkStatus::Token,
        Ok(_) => RtkStatus::Wrong,
        Err(_) => RtkStatus::Missing,
    }
}

fn ensure_rtk(yes: bool) -> Result<()> {
    match detect_rtk() {
        RtkStatus::Token => {
            println!("rtk: ok (Token Killer on PATH)");
            Ok(())
        }
        RtkStatus::Wrong => {
            eprintln!();
            eprintln!(
                "rtk: a different `rtk` binary resolves on PATH (likely \
                 reachingforthejack/rtk — \"Rust Type Kit\")."
            );
            eprintln!(
                "     Crabcc expects `rtk` = Rust Token Killer. Resolve \
                 the collision manually before re-running install-claude."
            );
            eprintln!("     See ~/.claude/RTK.md for details.");
            Ok(())
        }
        RtkStatus::Missing => {
            println!();
            println!(
                "rtk: not found on PATH (Rust Token Killer — \
                 hook-based shell-output token proxy)."
            );
            if !yes && !confirm("  Install via `cargo install rtk`? [y/N] ")? {
                println!("  skipped — install later with `cargo install rtk`");
                return Ok(());
            }
            install_rtk_via_cargo()
        }
    }
}

fn install_rtk_via_cargo() -> Result<()> {
    println!("  running: cargo install rtk");
    let status = Command::new("cargo")
        .args(["install", "rtk"])
        .status()
        .context("failed to invoke `cargo` — is it on PATH?")?;
    if !status.success() {
        bail!("`cargo install rtk` exited non-zero — install manually and re-run");
    }
    println!("  rtk: installed");
    Ok(())
}

/// Detect the user's login shell and return the appropriate rc/profile file.
fn shell_profile() -> PathBuf {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    if shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bash_profile")
    }
}

/// Append `line` to the user's shell profile (creates the file if absent).
fn append_to_profile(line: &str) -> Result<()> {
    use std::io::Write;
    let profile = shell_profile();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&profile)
        .with_context(|| format!("open {}", profile.display()))?;
    writeln!(f, "\n{line}")?;
    println!("  written to {}: {}", profile.display(), line);
    Ok(())
}

/// Check that `opencode` is reachable. If it lives at `~/.opencode/bin` but is
/// not on PATH, offer to patch the shell profile.
pub fn ensure_opencode(yes: bool) -> Result<()> {
    if Command::new("opencode")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or_default()
    {
        println!("opencode: ok (on PATH)");
        return Ok(());
    }

    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    let opencode_bin = home.join(".opencode/bin");
    println!();
    if opencode_bin.join("opencode").exists() {
        println!(
            "opencode: found at {} but not on PATH \
             (needed for orchestrator subagents).",
            opencode_bin.display()
        );
        if !yes && !confirm("  Add ~/.opencode/bin to PATH in shell profile? [y/N] ")? {
            println!("  skipped — add manually: export PATH=\"$HOME/.opencode/bin:$PATH\"");
            return Ok(());
        }
        append_to_profile("export PATH=\"$HOME/.opencode/bin:$PATH\"  # crabcc install-claude")?;
        println!("  restart your shell or run: export PATH=\"$HOME/.opencode/bin:$PATH\"");
    } else {
        println!("opencode: not installed — get it from https://opencode.ai");
        println!("  (required to dispatch orchestrator subagents)");
    }
    Ok(())
}

/// Parse the major version from `bash --version` output.
fn bash_major_version() -> u32 {
    let out = Command::new("bash").arg("--version").output();
    let s = match out {
        Ok(o) => String::from_utf8(o.stdout).unwrap_or_default(),
        Err(_) => return 0,
    };
    s.lines()
        .next()
        .and_then(|l| l.split("version ").nth(1))
        .and_then(|v| v.split('.').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_default()
}

/// Ensure bash 5+ is available. macOS ships bash 3.2 which lacks associative
/// arrays (`declare -A`) used by `orch-dispatch-wave`. If homebrew bash is
/// installed at `/opt/homebrew/bin/bash`, offer to prepend `/opt/homebrew/bin`
/// to PATH so `env bash` resolves to bash 5. If not installed, offer to run
/// `brew install bash`.
fn ensure_bash5(yes: bool) -> Result<()> {
    let ver = bash_major_version();
    if ver >= 5 {
        println!("bash: ok (v{ver}+, orchestrator dispatch supported)");
        return Ok(());
    }

    let brew_bash = std::path::Path::new("/opt/homebrew/bin/bash");
    println!();
    if ver > 0 {
        println!(
            "bash: system version is {ver} (< 5) — \
             `orch-dispatch-wave` needs bash 5+ for associative arrays"
        );
    }

    if brew_bash.exists() {
        println!("  homebrew bash 5 already installed at /opt/homebrew/bin/bash");
        if !yes && !confirm(
            "  Prepend /opt/homebrew/bin to PATH in shell profile (makes env bash → bash5)? [y/N] ",
        )? {
            println!(
                "  skipped — call dispatch manually with: \
                 /opt/homebrew/bin/bash $(which orch-dispatch-wave)"
            );
            return Ok(());
        }
        append_to_profile(
            "export PATH=\"/opt/homebrew/bin:$PATH\"  # crabcc install-claude (bash5 for orchestrator)",
        )?;
        println!("  restart your shell or run: export PATH=\"/opt/homebrew/bin:$PATH\"");
    } else {
        println!("  bash 5 not found — installing via `brew install bash`");
        if !yes && !confirm("  Run `brew install bash`? [y/N] ")? {
            println!("  skipped — install later with: brew install bash");
            return Ok(());
        }
        let status = Command::new("brew")
            .args(["install", "bash"])
            .status()
            .context("`brew` not found — install Homebrew first: https://brew.sh")?;
        if !status.success() {
            bail!("`brew install bash` failed — install manually and re-run");
        }
        append_to_profile(
            "export PATH=\"/opt/homebrew/bin:$PATH\"  # crabcc install-claude (bash5 for orchestrator)",
        )?;
        println!("  bash 5 installed — restart your shell or run: export PATH=\"/opt/homebrew/bin:$PATH\"");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_honors_env_var() {
        // Unsafe: we mutate process env. Run serially via `--test-threads=1`
        // if this proves flaky in nextest.
        let prev = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("CLAUDE_CONFIG_DIR", "/tmp/custom-claude");
        assert_eq!(
            claude_config_dir().unwrap(),
            PathBuf::from("/tmp/custom-claude")
        );

        std::env::set_var("CLAUDE_CONFIG_DIR", "  ");
        // Whitespace-only should fall back to home/.claude.
        let home_fallback = claude_config_dir().unwrap();
        assert!(home_fallback.ends_with(".claude"));

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let default = claude_config_dir().unwrap();
        assert!(default.ends_with(".claude"));

        if let Some(p) = prev {
            std::env::set_var("CLAUDE_CONFIG_DIR", p);
        }
    }

    #[test]
    fn asset_repo_path_remaps_skills_to_skill() {
        // Claude config has `skills/` plural; the source repo has `skill/` singular.
        assert_eq!(
            asset_repo_path("skills/crabcc/SKILL.md"),
            PathBuf::from("skill/crabcc/SKILL.md")
        );
        // Other assets pass through unchanged.
        assert_eq!(
            asset_repo_path("commands/crabcc-init.md"),
            PathBuf::from("commands/crabcc-init.md")
        );
    }
}
