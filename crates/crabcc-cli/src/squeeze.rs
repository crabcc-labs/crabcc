//! `crabcc squeeze` — collapse streaming/progress noise from captured command
//! output. Reads stdin, writes the squeezed text to stdout. A cheap stdin
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

/// Final visible frame of a physical line: everything after the last `\r`
/// (an in-place redraw), or the whole line when there is none.
fn collapse_cr(line: &str) -> &str {
    match line.rsplit_once('\r') {
        Some((_, tail)) => tail,
        None => line,
    }
}

/// Squeeze `input`. `max_lines == 0` disables the head/tail window (only the
/// lossless reductions run).
pub fn squeeze(input: &str, max_lines: usize) -> String {
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
        let line = collapse_cr(raw[i]);
        let mut count = 1;
        while i + count < raw.len() && collapse_cr(raw[i + count]) == line {
            count += 1;
        }
        deduped.push(line.to_string());
        if count > 1 {
            deduped.push(format!("[repeated {count}x]"));
        }
        i += count;
    }

    // (3) head/tail window with error surfacing.
    let windowed = window(deduped, max_lines);

    let mut out = windowed.join("\n");
    if had_trailing_nl {
        out.push('\n');
    }
    out
}

/// Keep the first and last `max_lines / 2` lines; replace the middle with an
/// elision marker plus any signal (error/warn) lines from the elided region.
fn window(lines: Vec<String>, max_lines: usize) -> Vec<String> {
    if max_lines == 0 || lines.len() <= max_lines {
        return lines;
    }
    let half = (max_lines / 2).max(1);
    let head = &lines[..half];
    let tail = &lines[lines.len() - half..];
    let middle = &lines[half..lines.len() - half];

    let surfaced: Vec<&String> = middle.iter().filter(|l| is_signal(l)).collect();

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

/// `crabcc squeeze [--max-lines N]`: read stdin, squeeze, print to stdout.
/// Always emits something (passthrough-shaped: an already-tiny stream squeezes
/// to itself).
pub fn run(max_lines: usize) -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("read stdin")?;
    print!("{}", squeeze(&input, max_lines));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_carriage_return_redraws_to_final_frame() {
        // A progress bar redrawn in place collapses to its last frame.
        let s = squeeze("10%\r45%\r100%\ndone\n", 0);
        assert_eq!(s, "100%\ndone\n");
    }

    #[test]
    fn run_length_dedupes_consecutive_identical_lines() {
        let s = squeeze("waiting\nwaiting\nwaiting\nready\n", 0);
        assert_eq!(s, "waiting\n[repeated 3x]\nready\n");
    }

    #[test]
    fn lossless_reductions_leave_distinct_lines_untouched() {
        let s = squeeze("a\nb\nc\n", 0);
        assert_eq!(s, "a\nb\nc\n");
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
        let s = squeeze(&input, 6);
        assert!(s.contains("start"), "{s}");
        assert!(s.contains("end"), "{s}");
        assert!(s.contains("lines elided"), "{s}");
        // The error buried in the elided middle must still be surfaced.
        assert!(s.contains("ERROR: disk full"), "{s}");
    }

    #[test]
    fn no_trailing_newline_is_preserved() {
        assert_eq!(squeeze("a\nb", 0), "a\nb");
        assert_eq!(squeeze("a\nb\n", 0), "a\nb\n");
    }
}
