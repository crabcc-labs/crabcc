//! `crabcc squeeze` — collapse streaming/progress noise from captured command
//! output. Reads stdin, writes the squeezed text to stdout and a one-line
//! self-describing disclosure to stderr (so the agent always knows the output
//! was processed, why, and how to get the raw stream back). A cheap stdin
//! filter (no network, no blocking) for the token sink that build / install /
//! test / log streams create: most of their bytes are transient redraws and
//! repeated lines the agent re-reads for no new information.
//!
//! Three reductions, applied in order:
//!
//! 1. **Carriage-return collapse** - a physical line redrawn in place with
//!    `\r` (progress bars, spinners, download `%`) keeps only its final frame.
//! 2. **Run-length dedupe** - consecutive identical lines collapse to one line
//!    plus a `[repeated Nx]` marker (the original line is kept verbatim).
//! 3. **Head/tail window** (`max_lines > 0`) - keep the first and last lines
//!    verbatim and elide the middle, but always surface error/warning lines
//!    from the elided region so a real failure is never dropped.
//!
//! (1) and (2) are informationally lossless (transient frames / exact repeats);
//! (3) is lossy but disclosed (`[... N lines elided ...]`) and error-preserving.

use anyhow::{Context, Result};
use std::io::Read as _;

/// Substrings (case-insensitive) that mark a line as worth keeping even when
/// it falls in an elided window.
const SIGNAL: &[&str] = &[
    "error",
    "warn",
    "fail",
    "panic",
    "fatal",
    "traceback",
    "exception",
    "assert",
];

fn is_signal(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SIGNAL.iter().any(|k| lower.contains(k))
}

/// What the filter actually changed - drives the stderr disclosure so the
/// agent knows what happened and whether it should re-fetch the raw output.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct SqueezeStats {
    /// Physical lines that carried an in-place `\r` redraw (collapsed to the
    /// final frame).
    pub redraws_collapsed: usize,
    /// Runs of identical lines collapsed (each run had count > 1).
    pub repeated_groups: usize,
    /// Total identical lines removed by run-length dedupe.
    pub repeated_lines_removed: usize,
    /// Middle lines dropped by the `--max-lines` window.
    pub lines_elided: usize,
    /// Error/warning lines pulled out of the elided window and kept.
    pub signals_surfaced: usize,
}

impl SqueezeStats {
    fn touched(&self) -> bool {
        self.redraws_collapsed > 0 || self.repeated_lines_removed > 0 || self.lines_elided > 0
    }
}

/// Final visible frame of a physical line: everything after the last `\r`
/// (an in-place redraw), or the whole line when there is none.
fn collapse_cr(line: &str) -> &str {
    match line.rsplit_once('\r') {
        Some((_, tail)) => tail,
        None => line,
    }
}

/// Squeeze `input`, returning the reduced text plus what changed. `max_lines
/// == 0` disables the head/tail window (only the lossless reductions run).
pub fn squeeze(input: &str, max_lines: usize) -> (String, SqueezeStats) {
    let mut stats = SqueezeStats::default();

    // Preserve a trailing newline: `split('\n')` yields a final "" for it.
    let had_trailing_nl = input.ends_with('\n');
    let raw: Vec<&str> = {
        let mut v: Vec<&str> = input.split('\n').collect();
        if had_trailing_nl {
            v.pop(); // drop the empty element the trailing '\n' produced
        }
        v
    };

    // (1) carriage-return collapse, then (2) run-length dedupe.
    let mut deduped: Vec<String> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i].contains('\r') {
            stats.redraws_collapsed += 1;
        }
        let line = collapse_cr(raw[i]);
        let mut count = 1;
        while i + count < raw.len() && collapse_cr(raw[i + count]) == line {
            if raw[i + count].contains('\r') {
                stats.redraws_collapsed += 1;
            }
            count += 1;
        }
        deduped.push(line.to_string());
        if count > 1 {
            deduped.push(format!("[repeated {count}x]"));
            stats.repeated_groups += 1;
            stats.repeated_lines_removed += count - 1;
        }
        i += count;
    }

    // (3) head/tail window with error surfacing.
    let windowed = window(deduped, max_lines, &mut stats);

    let mut out = windowed.join("\n");
    if had_trailing_nl {
        out.push('\n');
    }
    (out, stats)
}

/// Keep the first and last `max_lines / 2` lines; replace the middle with an
/// elision marker plus any signal (error/warn) lines from the elided region.
fn window(lines: Vec<String>, max_lines: usize, stats: &mut SqueezeStats) -> Vec<String> {
    if max_lines == 0 || lines.len() <= max_lines {
        return lines;
    }
    let half = (max_lines / 2).max(1);
    let head = &lines[..half];
    let tail = &lines[lines.len() - half..];
    let middle = &lines[half..lines.len() - half];

    let surfaced: Vec<&String> = middle.iter().filter(|l| is_signal(l)).collect();
    stats.lines_elided = middle.len();
    stats.signals_surfaced = surfaced.len();

    let mut out: Vec<String> = Vec::with_capacity(head.len() + tail.len() + surfaced.len() + 2);
    out.extend(head.iter().cloned());
    out.push(format!(
        "[... {} lines elided; {} error/warning line(s) surfaced ...]",
        middle.len(),
        surfaced.len()
    ));
    out.extend(surfaced.into_iter().cloned());
    out.extend(tail.iter().cloned());
    out
}

/// One-line disclosure for stderr: tells the agent this was a `crabcc squeeze`
/// view, what it dropped, and how to get the raw stream. Always returned so
/// the agent never mistakes a squeezed view for the command's real output.
pub fn disclosure(stats: &SqueezeStats) -> String {
    if !stats.touched() {
        return "[crabcc squeeze] token-reduction filter ran; no progress/repeat noise found, \
                output is the command's raw stream unchanged."
            .into();
    }
    let mut what = Vec::new();
    if stats.redraws_collapsed > 0 {
        what.push(format!(
            "collapsed {} in-place progress redraw line(s)",
            stats.redraws_collapsed
        ));
    }
    if stats.repeated_lines_removed > 0 {
        what.push(format!(
            "removed {} repeated line(s) in {} run(s)",
            stats.repeated_lines_removed, stats.repeated_groups
        ));
    }
    if stats.lines_elided > 0 {
        what.push(format!(
            "elided {} middle line(s) ({} error/warning line(s) kept)",
            stats.lines_elided, stats.signals_surfaced
        ));
    }
    format!(
        "[crabcc squeeze] token-reduced view of the command's output: {}. \
         Errors/warnings preserved. Re-run the command without `| crabcc squeeze` for the raw stream.",
        what.join("; ")
    )
}

/// `crabcc squeeze [--max-lines N]`: read stdin, squeeze, print the reduced
/// text to stdout and the disclosure to stderr. Always emits something
/// (passthrough-shaped: an already-tiny stream squeezes to itself).
pub fn run(max_lines: usize) -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("read stdin")?;
    let (out, stats) = squeeze(&input, max_lines);
    print!("{out}");
    eprintln!("{}", disclosure(&stats));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(input: &str, max_lines: usize) -> String {
        squeeze(input, max_lines).0
    }

    #[test]
    fn collapses_carriage_return_redraws_to_final_frame() {
        // A progress bar redrawn in place collapses to its last frame.
        assert_eq!(sq("10%\r45%\r100%\ndone\n", 0), "100%\ndone\n");
    }

    #[test]
    fn run_length_dedupes_consecutive_identical_lines() {
        assert_eq!(
            sq("waiting\nwaiting\nwaiting\nready\n", 0),
            "waiting\n[repeated 3x]\nready\n"
        );
    }

    #[test]
    fn lossless_reductions_leave_distinct_lines_untouched() {
        let (out, stats) = squeeze("a\nb\nc\n", 0);
        assert_eq!(out, "a\nb\nc\n");
        assert!(!stats.touched());
    }

    #[test]
    fn window_elides_middle_but_surfaces_errors() {
        let mut input = String::from("start\n");
        for i in 0..50 {
            input.push_str(&format!("step {i}\n"));
        }
        input.push_str("ERROR: disk full\n");
        for i in 50..100 {
            input.push_str(&format!("step {i}\n"));
        }
        input.push_str("end\n");
        let (s, stats) = squeeze(&input, 6);
        assert!(s.contains("start") && s.contains("end"), "{s}");
        assert!(s.contains("lines elided"), "{s}");
        // The error buried in the elided middle must still be surfaced.
        assert!(s.contains("ERROR: disk full"), "{s}");
        assert!(
            stats.lines_elided > 0 && stats.signals_surfaced == 1,
            "{stats:?}"
        );
    }

    #[test]
    fn no_trailing_newline_is_preserved() {
        assert_eq!(sq("a\nb", 0), "a\nb");
        assert_eq!(sq("a\nb\n", 0), "a\nb\n");
    }

    #[test]
    fn disclosure_describes_changes_and_offers_raw() {
        let (_, stats) = squeeze("x\rY\ndup\ndup\n", 0);
        let d = disclosure(&stats);
        assert!(d.contains("crabcc squeeze"), "{d}");
        assert!(d.contains("redraw") && d.contains("repeated"), "{d}");
        assert!(d.contains("without `| crabcc squeeze`"), "{d}");
        // Clean input -> "unchanged" disclosure.
        let (_, clean) = squeeze("a\nb\n", 0);
        assert!(
            disclosure(&clean).contains("unchanged"),
            "{}",
            disclosure(&clean)
        );
    }
}
