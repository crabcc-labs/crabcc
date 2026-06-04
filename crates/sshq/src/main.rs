//! `sshq` — a token-thrifty `ssh` wrapper for AI coding agents.
//!
//! See `Cargo.toml` for the five optimizations this bakes in. The whole
//! tool is a single pass: parse flags → build a remote command string →
//! assemble the `ssh` argv → exec. No background state, no config file.

#![forbid(unsafe_code)]

use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Parser;

/// Token-thrifty `ssh` wrapper: clean output, remote-side filtering,
/// connection multiplexing, and escape-free script piping.
#[derive(Parser, Debug)]
#[command(
    name = "sshq",
    version,
    about,
    // Mirror ssh's own grammar: flags first, then HOST, then the command
    // (everything after HOST is captured verbatim as the remote command).
    override_usage = "sshq [OPTIONS] <HOST> [COMMAND]..."
)]
struct Cli {
    /// Target host: `user@host`, `host`, or an `~/.ssh/config` alias.
    host: String,

    /// Command to run remotely. Joined with spaces and handed to the
    /// remote shell, exactly like `ssh HOST <command>`. Omit it for an
    /// interactive shell, or when using `--script`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,

    /// Keep only the last N lines (remote-side `tail -n N`).
    #[arg(long, value_name = "N")]
    tail: Option<u64>,

    /// Keep only lines matching PATTERN (remote-side `grep -e PATTERN`).
    #[arg(long, value_name = "PATTERN")]
    grep: Option<String>,

    /// Return just a line count, not the lines (remote-side `wc -l`).
    /// Composes with `--grep` to count matches.
    #[arg(long)]
    count: bool,

    /// Stream a multi-line script to a remote `bash -s` over stdin,
    /// avoiding all quote-escaping. Use `-` to read the script from this
    /// process's stdin. Mutually exclusive with a positional COMMAND.
    #[arg(long, value_name = "FILE")]
    script: Option<String>,

    /// SSH port (`ssh -p`).
    #[arg(short = 'p', long, value_name = "PORT")]
    port: Option<u16>,

    /// Allocate a TTY and allow interactive auth: drops `-T`, `-q` and
    /// `BatchMode=yes`. Use for remote TUIs or passphrase prompts.
    #[arg(long)]
    tty: bool,

    /// Keep stderr separate (skip the `2>&1` merge before filtering).
    #[arg(long)]
    no_merge: bool,

    /// Keep color: skip the `NO_COLOR=1 TERM=dumb CI=1` env injection.
    #[arg(long)]
    color: bool,

    /// Disable connection multiplexing (no ControlMaster/Path/Persist).
    #[arg(long)]
    no_mux: bool,

    /// How long the multiplexed master socket lingers after the last
    /// session closes (`ControlPersist`).
    #[arg(long, value_name = "DURATION", default_value = "10m")]
    persist: String,

    /// Extra `-o KEY=VALUE` passed verbatim to ssh. Repeatable.
    #[arg(short = 'o', value_name = "KEY=VALUE")]
    ssh_opt: Vec<String>,

    /// Print the `ssh` invocation (shell-quoted, copy-pasteable) instead
    /// of running it.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.script.is_some() && !cli.command.is_empty() {
        bail!("--script and a positional COMMAND are mutually exclusive");
    }

    // No command and no script → the user wants an interactive shell.
    // Honour that by forcing TTY semantics and skipping the
    // agent-output massaging (a human is driving).
    let interactive = cli.command.is_empty() && cli.script.is_none();
    let tty = cli.tty || interactive;

    let remote = build_remote_command(&cli, interactive);
    let args = build_ssh_args(&cli, tty, remote.as_deref())?;

    if cli.dry_run {
        let mut line = String::from("ssh");
        for a in &args {
            line.push(' ');
            line.push_str(&shell_quote(a));
        }
        println!("{line}");
        return Ok(());
    }

    let mut cmd = Command::new("ssh");
    cmd.args(&args);

    // `--script FILE` pipes the file into ssh's stdin so the remote
    // `bash -s` reads it. `--script -` (and the no-script paths) inherit
    // our stdin unchanged.
    if let Some(script) = &cli.script {
        if script != "-" {
            let f = File::open(script).with_context(|| format!("opening script file {script}"))?;
            cmd.stdin(Stdio::from(f));
        }
    }

    let status = cmd
        .status()
        .context("failed to exec ssh (is it on PATH?)")?;
    // Propagate the remote command's exit code so callers/agents can
    // branch on success vs failure.
    std::process::exit(status.code().unwrap_or(1));
}

/// Build the remote command string ssh will hand to the remote shell.
/// Returns `None` for an interactive session (no command, no script).
fn build_remote_command(cli: &Cli, interactive: bool) -> Option<String> {
    if interactive {
        return None;
    }

    // Item 3: env injection. Prepended as an `export` so it applies to
    // the whole pipeline, not just the first stage.
    let mut s = String::new();
    if !cli.color {
        s.push_str("export NO_COLOR=1 TERM=dumb CI=1; ");
    }

    // The payload: either the user's command (verbatim, ssh-style) or a
    // remote `bash -s` that consumes the piped script on stdin.
    if cli.script.is_some() {
        s.push_str("bash -s");
    } else {
        s.push_str(&cli.command.join(" "));
    }

    // No filter → the command (or `bash -s`) is the last statement, so
    // ssh already returns its exit status. Hand it over untouched.
    let has_filter = cli.tail.is_some() || cli.grep.is_some() || cli.count;
    if !has_filter {
        return Some(s);
    }

    // Item 2: remote-side filtering pipeline. Order is fixed and
    // documented: merge stderr → grep → tail → count.
    if !cli.no_merge {
        s.push_str(" 2>&1");
    }
    if let Some(pat) = &cli.grep {
        s.push_str(" | grep -e ");
        s.push_str(&shell_quote(pat));
    }
    if let Some(n) = cli.tail {
        s.push_str(&format!(" | tail -n {n}"));
    }
    if cli.count {
        s.push_str(" | wc -l");
    }

    // A pipeline's exit status is its *last* stage's, which would mask
    // the user command's result — `tail` always succeeds (hiding a
    // failed `cargo test`), and `grep` exits 1 on no match (turning a
    // success into a failure). Re-exit with the first stage's status
    // (`PIPESTATUS[0]`, the user command) so the advertised
    // exit-code propagation holds. `PIPESTATUS` is a bashism, so run the
    // whole thing under `bash -c` rather than trust the remote login
    // shell.
    s.push_str("; exit ${PIPESTATUS[0]}");
    Some(format!("bash -c {}", shell_quote(&s)))
}

/// Assemble the full `ssh` argv: optimization flags, multiplexing,
/// passthrough options, host, and the remote command.
fn build_ssh_args(cli: &Cli, tty: bool, remote: Option<&str>) -> Result<Vec<String>> {
    let mut a: Vec<String> = Vec::new();

    // Item 1: clean, unstyled output for agents. For interactive/TTY
    // sessions do the opposite — force a PTY and stay chatty enough to
    // show auth prompts.
    if tty {
        a.push("-t".into());
    } else {
        a.push("-T".into());
        a.push("-q".into());
        opt(&mut a, "BatchMode=yes");
    }

    // Sensible non-interactive defaults regardless of mode.
    opt(&mut a, "StrictHostKeyChecking=accept-new");
    opt(&mut a, "ServerAliveInterval=60");
    opt(&mut a, "ServerAliveCountMax=3");

    // Item 4: connection multiplexing. `%C` (a hash of the connection
    // tuple) keeps the socket path short — `%r@%h:%p` can blow past the
    // ~104-char unix-socket limit on long hostnames.
    if !cli.no_mux {
        let dir = sockets_dir()?;
        // Keep `--dry-run` side-effect-free; the real run creates it.
        if !cli.dry_run {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating socket dir {}", dir.display()))?;
        }
        opt(&mut a, "ControlMaster=auto");
        opt(&mut a, &format!("ControlPath={}/%C", dir.display()));
        opt(&mut a, &format!("ControlPersist={}", cli.persist));
    }

    if let Some(p) = cli.port {
        a.push("-p".into());
        a.push(p.to_string());
    }
    for o in &cli.ssh_opt {
        opt(&mut a, o);
    }

    a.push(cli.host.clone());
    if let Some(r) = remote {
        a.push(r.to_string());
    }
    Ok(a)
}

/// Push a `-o KEY=VALUE` pair onto the ssh argv.
fn opt(args: &mut Vec<String>, kv: &str) {
    args.push("-o".into());
    args.push(kv.to_string());
}

/// `~/.ssh/sockets`, where multiplexed master sockets live.
fn sockets_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let mut p = PathBuf::from(home);
    p.push(".ssh");
    p.push("sockets");
    Ok(p)
}

/// POSIX single-quote escaping so a value survives the remote shell
/// verbatim. Bare word when safe; `'...'` (with `'\''` for embedded
/// quotes) otherwise.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-/:=@%+,".contains(c))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        let mut v = vec!["sshq"];
        v.extend_from_slice(args);
        Cli::parse_from(v)
    }

    #[test]
    fn plain_command_is_passed_through() {
        let c = cli(&["host", "cargo", "build"]);
        let r = build_remote_command(&c, false).unwrap();
        assert!(r.ends_with("cargo build"));
        assert!(r.starts_with("export NO_COLOR=1 TERM=dumb CI=1; "));
    }

    #[test]
    fn color_flag_drops_env_prefix() {
        let c = cli(&["--color", "host", "ls"]);
        let r = build_remote_command(&c, false).unwrap();
        assert_eq!(r, "ls");
    }

    #[test]
    fn filter_pipeline_order() {
        let c = cli(&["--grep", "ERROR", "--tail", "5", "--count", "host", "make"]);
        let r = build_remote_command(&c, false).unwrap();
        // Filtering wraps in `bash -c` and re-exits with the user
        // command's status so the pipeline doesn't mask it.
        assert!(r.starts_with("bash -c "));
        assert!(r.contains("make 2>&1 | grep -e ERROR | tail -n 5 | wc -l; exit ${PIPESTATUS[0]}"));
    }

    #[test]
    fn unfiltered_command_keeps_its_own_exit_status() {
        // No filter → no pipeline, no bash -c wrapper: ssh returns the
        // command's status directly.
        let c = cli(&["host", "cargo", "test"]);
        let r = build_remote_command(&c, false).unwrap();
        assert!(!r.contains("PIPESTATUS"));
        assert!(!r.starts_with("bash -c"));
    }

    #[test]
    fn grep_pattern_is_quoted() {
        let c = cli(&["--grep", "a b|c", "host", "make"]);
        let r = build_remote_command(&c, false).unwrap();
        // The pattern survives the (now nested) quoting verbatim.
        assert!(r.contains("a b|c"));
        assert!(r.starts_with("bash -c "));
    }

    #[test]
    fn script_mode_uses_bash_s() {
        let c = cli(&["--script", "deploy.sh", "host"]);
        let r = build_remote_command(&c, false).unwrap();
        assert!(r.contains("bash -s"));
    }

    #[test]
    fn interactive_has_no_remote_command() {
        let c = cli(&["host"]);
        assert!(build_remote_command(&c, true).is_none());
    }

    #[test]
    fn agent_args_are_clean() {
        let c = cli(&["host", "ls"]);
        let args = build_ssh_args(&c, false, Some("ls")).unwrap();
        assert!(args.contains(&"-T".to_string()));
        assert!(args.contains(&"-q".to_string()));
        assert!(args.iter().any(|a| a == "BatchMode=yes"));
        assert!(args.iter().any(|a| a == "ControlMaster=auto"));
    }

    #[test]
    fn tty_mode_forces_pty_and_drops_batch() {
        let c = cli(&["--tty", "host", "top"]);
        let args = build_ssh_args(&c, true, Some("top")).unwrap();
        assert!(args.contains(&"-t".to_string()));
        assert!(!args.contains(&"-T".to_string()));
        assert!(!args.iter().any(|a| a == "BatchMode=yes"));
    }

    #[test]
    fn no_mux_omits_control_options() {
        let c = cli(&["--no-mux", "host", "ls"]);
        let args = build_ssh_args(&c, false, Some("ls")).unwrap();
        assert!(!args.iter().any(|a| a.starts_with("ControlMaster")));
    }

    #[test]
    fn shell_quote_handles_specials() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
