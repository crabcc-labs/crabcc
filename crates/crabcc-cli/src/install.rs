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

/// Caller-provided flags for `crabcc install-claude`. Issue #105 Phase 5b
/// added the two `with_ollama_stack` / `print_stack_instructions` fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    pub yes: bool,
    pub print_hooks_only: bool,
    pub with_ollama_stack: bool,
    pub print_stack_instructions: bool,
}

pub fn run(opts: InstallOptions) -> Result<()> {
    if opts.print_hooks_only {
        // Round-trip through serde_json so output is canonicalised pretty JSON.
        let v: serde_json::Value = serde_json::from_str(HOOKS_JSON)
            .context("embedded hooks-claude.json is not valid JSON")?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    if opts.print_stack_instructions {
        return print_stack_instructions();
    }

    let yes = opts.yes;
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
        (
            repo_root.join("commands/crabcc/generate/context.md"),
            home.join(".claude/commands/crabcc/generate/context.md"),
        ),
    ];

    for (src, dst) in pairs {
        if !src.exists() {
            bail!("source missing: {}", src.display());
        }
        link_one(src, dst, yes)?;
    }

    // RTK (Rust Token Killer) is a runtime dependency for the
    // hook-based shell-output proxy that keeps token use lean inside
    // Claude Code. We don't bake it into the binary — `cargo install
    // rtk` keeps it upgradable on its own cadence — but the bootstrap
    // surface is the right place to check + prompt.
    ensure_rtk(yes)?;

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

    if opts.with_ollama_stack {
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

    Ok(())
}

/// Phase 5b — write the embedded ` install/ollama-stack/` files to
/// ` $HOME/.crabcc/ollama-stack/` so the user has a writable, OS-local
/// copy of the Compose recipe. Idempotent — overwrites on each call so
/// upgrading crabcc picks up Caddyfile / docker-compose.yml changes.
/// Preserves any existing ` .env` (real secrets) by skipping it when
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

    // Don't clobber a real .env. We never embed one.
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

/// Print the bring-up commands for the Ollama stack without symlinking
/// any Claude config. Counterpart to ` --print-hooks` for the
/// ` install/ollama-stack/` recipe.
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

/// Result of probing PATH for `rtk`. The name collides between the
/// wanted "Rust Token Killer" (rtk gain / rtk discover / rtk proxy)
/// and reachingforthejack/rtk ("Rust Type Kit"); the latter has no
/// `gain` subcommand, so we use that as the discriminator. The user's
/// global RTK.md (loaded into every Claude Code session) calls out
/// the same collision — keep this aligned with it.
#[derive(Debug, PartialEq, Eq)]
enum RtkStatus {
    /// Correct rtk on PATH (Token Killer).
    Token,
    /// Some other `rtk` resolves on PATH but doesn't respond to
    /// `rtk gain` — likely Rust Type Kit. We don't auto-overwrite —
    /// the user must resolve the collision manually.
    Wrong,
    /// No `rtk` on PATH.
    Missing,
}

fn detect_rtk() -> RtkStatus {
    // Token Killer responds to `rtk gain`; Type Kit does not. A
    // success on `rtk gain --help` is the most discriminating fast
    // probe (no state changes, no network).
    let probe = Command::new("rtk").arg("gain").arg("--help").output();
    match probe {
        Ok(out) if out.status.success() => RtkStatus::Token,
        Ok(_) => RtkStatus::Wrong,
        Err(_) => RtkStatus::Missing,
    }
}

/// Ensure the Token-Killer `rtk` is installed and on PATH. Prompts
/// with `cargo install rtk` when missing; warns and exits cleanly on
/// the binary-name collision rather than auto-clobbering. Honours
/// `--yes` for the missing case (auto-installs without prompt).
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
            // Non-fatal — the rest of the install still works.
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
