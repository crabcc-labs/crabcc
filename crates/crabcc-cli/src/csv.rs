//! `crabcc csv <stats|sample|count> <file>` — token-cheap CSV summaries via
//! [qsv](https://github.com/dathere/qsv).
//!
//! Deliberately an **explicit** subcommand, not a shell rewrite: `stats` and
//! `sample` return a *summary*, not the rows the caller asked for, so unlike
//! the lossless `cat`/`grep` rewrites this must be invoked on purpose. When
//! `qsv` is absent it errors with an install hint rather than silently
//! dropping data. The intent is to give an agent a cheap alternative to
//! `cat`ting a large CSV into context.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsvOp {
    /// Per-column stats (`qsv stats`): type, min/max, mean, etc.
    Stats,
    /// `n` random rows (`qsv sample`).
    Sample,
    /// Row count (`qsv count`).
    Count,
}

/// The qsv argv (without the leading `qsv`) for `op` over `file`. `n` is the
/// sample size, used only by [`CsvOp::Sample`]. Pure so it is unit-testable
/// without qsv installed.
fn qsv_args(op: CsvOp, file: &str, n: usize) -> Vec<String> {
    match op {
        CsvOp::Stats => vec!["stats".into(), file.into()],
        CsvOp::Sample => vec!["sample".into(), n.to_string(), file.into()],
        CsvOp::Count => vec!["count".into(), file.into()],
    }
}

pub fn run(op: CsvOp, file: &Path, n: usize) -> Result<()> {
    if !tool_on_path("qsv") {
        bail!(
            "`qsv` not found on PATH. Install it (https://github.com/dathere/qsv) for \
             token-cheap CSV summaries, or read the file directly with `cat` / `crabcc read`."
        );
    }
    if !file.is_file() {
        bail!("{} is not a readable file", file.display());
    }
    let args = qsv_args(op, &file.to_string_lossy(), n);
    // Inherit stdio: qsv's summary streams straight to the caller; crabcc only
    // adds tool gating + a stable, discoverable entry point.
    let status = Command::new("qsv")
        .args(&args)
        .status()
        .with_context(|| "run qsv")?;
    if !status.success() {
        bail!("qsv {} exited with {status}", args[0]);
    }
    Ok(())
}

/// True if an executable named `name` is on `$PATH`. A plain PATH walk (no
/// child spawn) so a presence check has no side effects.
fn tool_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(name)))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qsv_args_per_op() {
        assert_eq!(qsv_args(CsvOp::Stats, "d.csv", 0), ["stats", "d.csv"]);
        assert_eq!(qsv_args(CsvOp::Count, "d.csv", 0), ["count", "d.csv"]);
        assert_eq!(
            qsv_args(CsvOp::Sample, "d.csv", 25),
            ["sample", "25", "d.csv"]
        );
    }

    #[test]
    fn tool_on_path_finds_a_known_binary_and_misses_a_fake_one() {
        // `sh` is on PATH in every environment this runs in; the random name
        // is not. Exercises the PATH walk + executable check both ways.
        assert!(tool_on_path("sh"));
        assert!(!tool_on_path("crabcc-no-such-tool-zzz"));
    }
}
