//! Safe, conservative rewriting of agent-issued shell commands into
//! cheaper modern equivalents for the Claude Code PreToolUse Bash hook.
//!
//! The hook calls `crabcc shell rewrite --command "<cmd>"`; when this
//! recognises a *provably safe* rewrite it prints the Claude Code
//! `hookSpecificOutput.updatedInput` envelope on stdout so the rewritten
//! command runs transparently in place of the original. Otherwise it
//! prints nothing and the original command runs unchanged.
//!
//! Two rewrite families, both gated for safety:
//!
//!   * **Faithful engine swap** — `grep -rn P` -> `rg -n P`,
//!     `find PATH -name GLOB` -> `rg --files -g GLOB PATH`. ripgrep is a
//!     faithful superset of grep for literal line search and skips
//!     `.gitignore`'d / hidden paths, which is where the token bloat
//!     lives (`target/`, `node_modules/`). Only applied when the pattern
//!     is regex-compatible between grep-BRE and ripgrep, and only when
//!     `rg` is actually on PATH.
//!   * **Symbol upgrade** — `grep IDENT` / `rg IDENT` -> `crabcc refs
//!     IDENT`, but *only* when IDENT is a bare identifier confirmed to be
//!     an indexed symbol and the search is repo-wide. The header
//!     discloses the symbol scope and the raw-text `rg` fallback so the
//!     model never silently loses comment/doc matches.
//!
//! Anything we do not model (perl regex, context flags, pipes, `-exec`,
//! redirects, command substitution) passes through untouched — the rule
//! is "rewrite only when certain, else do nothing". Set
//! `CRABCC_NO_REWRITE=1` to disable rewriting entirely.

use anyhow::Result;
use crabcc_core::{store::Store, track};
use std::cell::RefCell;
use std::path::Path;

/// A planned rewrite of a single shell command. `inner` is the bare
/// replacement command (no provenance header — that is added at emit
/// time so tests can assert the replacement directly).
#[derive(Debug, PartialEq, Eq)]
pub struct Rewrite {
    /// The replacement command, fully quoted and ready to run.
    pub inner: String,
    /// Stable rule id for tracing + header (e.g. "grep->rg").
    pub rule: &'static str,
    /// Caveat surfaced in the output header (e.g. the rg fallback).
    pub note: Option<String>,
    /// `crabcc track` op this rewrite is accounted under: "refs" for
    /// symbol upgrades (reuses the calibrated grep-for-symbol estimate),
    /// "rewrite" for faithful swaps (counted, no fabricated savings).
    pub track_op: &'static str,
}

/// Shell metacharacters that change semantics or make naive tokenisation
/// unsafe. Their presence anywhere in the command forces passthrough.
const META: &[char] = &[
    '|', '&', ';', '<', '>', '$', '`', '(', ')', '{', '}', '\\', '\n',
];

/// Split a command into argv, honouring single/double quotes. Returns
/// `None` (passthrough) on any shell metacharacter or unbalanced quote —
/// we only ever rewrite single, simple commands.
fn tokenize(cmd: &str) -> Option<Vec<String>> {
    if cmd.chars().any(|c| META.contains(&c)) {
        return None;
    }
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut quote: Option<char> = None;
    for c in cmd.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_tok = true;
                }
                c if c.is_whitespace() => {
                    if in_tok {
                        tokens.push(std::mem::take(&mut cur));
                        in_tok = false;
                    }
                }
                c => {
                    cur.push(c);
                    in_tok = true;
                }
            },
        }
    }
    if quote.is_some() {
        return None; // unbalanced quote
    }
    if in_tok {
        tokens.push(cur);
    }
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

/// A bare identifier: the only pattern shape safe to upgrade to a
/// symbol query. Length >= 2 to avoid noise on single letters.
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    s.len() >= 2 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// True if a literal-intent pattern is regex-compatible between grep's
/// default BRE and ripgrep's Rust-regex. We have already rejected
/// `( ) { } \ $` at tokenise time; the remaining divergent chars are
/// `+` and `?` (literal in BRE, operators in ripgrep). `. * [ ] ^` mean
/// the same in both, so they are safe to carry across.
fn regex_compatible(pattern: &str) -> bool {
    !pattern.contains('+') && !pattern.contains('?')
}

/// Single-quote a token for safe interpolation into a shell command,
/// leaving simple tokens bare for readability.
fn shq(s: &str) -> String {
    let simple = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/=:,@".contains(c));
    if simple {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Plan a rewrite for `cmd`. `is_symbol` is consulted only for the
/// bare-identifier grep/rg case, so the (DB-backed) predicate is never
/// invoked for the common non-search command.
pub fn plan(cmd: &str, is_symbol: &dyn Fn(&str) -> bool) -> Option<Rewrite> {
    let toks = tokenize(cmd)?;
    let (prog, rest) = toks.split_first()?;
    match prog.as_str() {
        "grep" => plan_grep(rest, is_symbol),
        "rg" => plan_rg(rest, is_symbol),
        "find" => plan_find(rest),
        _ => None,
    }
}

#[derive(Default)]
struct GrepOpts {
    recursive: bool,
    line_numbers: bool,
    files_only: bool,
    count: bool,
    ignore_case: bool,
    word: bool,
    fixed: bool,
    positionals: Vec<String>,
}

/// Parse the conservative grep/rg short-flag subset we understand.
/// Returns `None` on any unknown or argument-taking flag.
fn parse_short_flags(args: &[String], allow_recursive: bool) -> Option<GrepOpts> {
    let mut o = GrepOpts::default();
    for a in args {
        if a.starts_with("--") {
            return None; // long flags: passthrough
        }
        if a.len() > 1 && a.starts_with('-') {
            for ch in a[1..].chars() {
                match ch {
                    'r' | 'R' if allow_recursive => o.recursive = true,
                    'I' | 's' | 'H' | 'h' => {} // no-ops vs ripgrep defaults
                    'n' => o.line_numbers = true,
                    'l' => o.files_only = true,
                    'c' => o.count = true,
                    'i' => o.ignore_case = true,
                    'w' => o.word = true,
                    'F' => o.fixed = true,
                    _ => return None, // unknown flag -> passthrough
                }
            }
        } else {
            o.positionals.push(a.clone());
        }
    }
    Some(o)
}

fn plan_grep(args: &[String], is_symbol: &dyn Fn(&str) -> bool) -> Option<Rewrite> {
    let o = parse_short_flags(args, true)?;
    let (pattern, paths) = o.positionals.split_first()?;
    // No recursive flag and no explicit path == grep reads stdin; rewriting
    // to ripgrep (which scans `.`) would change behaviour. Passthrough.
    if !o.recursive && paths.is_empty() {
        return None;
    }
    let repo_wide = paths.is_empty() || (paths.len() == 1 && paths[0] == ".");

    // Symbol upgrade: repo-wide, case-sensitive, non-count search for a
    // bare identifier that is actually indexed.
    if repo_wide && !o.ignore_case && !o.count && is_bare_ident(pattern) && is_symbol(pattern) {
        return Some(symbol_upgrade(pattern, o.files_only, "grep->crabcc-refs"));
    }

    // Faithful ripgrep swap — only when the pattern is regex-compatible
    // (or a fixed string).
    if !o.fixed && !regex_compatible(pattern) {
        return None;
    }
    Some(rg_swap(&o, pattern, paths))
}

fn plan_rg(args: &[String], is_symbol: &dyn Fn(&str) -> bool) -> Option<Rewrite> {
    // ripgrep is already recursive; we only *upgrade* an rg search for a
    // bare indexed symbol to the precise crabcc query. rg->rg is a no-op.
    let o = parse_short_flags(args, false)?;
    let (pattern, paths) = o.positionals.split_first()?;
    let repo_wide = paths.is_empty() || (paths.len() == 1 && paths[0] == ".");
    if repo_wide && !o.ignore_case && !o.count && is_bare_ident(pattern) && is_symbol(pattern) {
        return Some(symbol_upgrade(pattern, o.files_only, "rg->crabcc-refs"));
    }
    None
}

fn plan_find(args: &[String]) -> Option<Rewrite> {
    // Only the `find PATH... -name GLOB [-type f]` shape maps cleanly to
    // `rg --files -g GLOB PATH`. Any other predicate -> passthrough.
    let mut paths = Vec::new();
    let mut glob: Option<String> = None;
    let mut iglob = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-name" | "-iname" => {
                if glob.is_some() {
                    return None; // multiple -name: too complex
                }
                glob = Some(args.get(i + 1)?.clone());
                iglob = a == "-iname";
                i += 2;
            }
            "-type" => {
                if args.get(i + 1)?.as_str() != "f" {
                    return None; // only plain files map to `rg --files`
                }
                i += 2;
            }
            s if s.starts_with('-') => return None, // unknown predicate
            s => {
                paths.push(s.to_string());
                i += 1;
            }
        }
    }
    let glob = glob?;
    let flag = if iglob { "--iglob" } else { "-g" };
    let mut inner = format!("rg --files {flag} {}", shq(&glob));
    for p in &paths {
        inner.push(' ');
        inner.push_str(&shq(p));
    }
    Some(Rewrite {
        inner,
        rule: "find->rg",
        note: Some("ripgrep --files: skips .gitignore'd and hidden paths".into()),
        track_op: "rewrite",
    })
}

fn symbol_upgrade(pattern: &str, files_only: bool, rule: &'static str) -> Rewrite {
    // Lookups live under the `lookup` parent command (`crabcc lookup refs`),
    // not at the top level.
    let mut inner = format!("crabcc lookup refs {}", shq(pattern));
    if files_only {
        inner.push_str(" --files-only");
    }
    Rewrite {
        inner,
        rule,
        note: Some(format!(
            "symbol-scoped code refs; for raw text (comments/docs) use: rg {}",
            shq(pattern)
        )),
        track_op: "refs",
    }
}

fn rg_swap(o: &GrepOpts, pattern: &str, paths: &[String]) -> Rewrite {
    let mut inner = String::from("rg");
    if o.ignore_case {
        inner.push_str(" -i");
    }
    if o.word {
        inner.push_str(" -w");
    }
    if o.fixed {
        inner.push_str(" -F");
    }
    if o.line_numbers {
        inner.push_str(" -n");
    }
    if o.files_only {
        inner.push_str(" -l");
    }
    if o.count {
        inner.push_str(" -c");
    }
    inner.push(' ');
    inner.push_str(&shq(pattern));
    for p in paths {
        inner.push(' ');
        inner.push_str(&shq(p));
    }
    Rewrite {
        inner,
        rule: "grep->rg",
        note: Some(
            "ripgrep: skips .gitignore'd and hidden paths (grep --no-ignore to include)".into(),
        ),
        track_op: "rewrite",
    }
}

/// Is `rg` on PATH? Faithful swaps must never emit a command the agent's
/// environment cannot run, or the rewrite itself becomes the error.
fn rg_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join("rg").is_file())
}

/// Build the one-line provenance header prepended to the rewritten
/// command's output.
fn header(rw: &Rewrite, saved: usize) -> String {
    let mut h = format!("## crabcc-rewrite [{}]", rw.rule);
    if saved > 0 {
        h.push_str(&format!(" ~{saved} tok saved (est)"));
    }
    if let Some(n) = &rw.note {
        h.push_str(" - ");
        h.push_str(n);
    }
    h
}

/// Hook entry point. Resolves the symbol predicate lazily against the
/// repo index, plans a rewrite, records it (trace + ledger) and prints
/// the Claude Code PreToolUse envelope. Prints nothing when there is no
/// safe rewrite.
pub fn run(root: &Path, db: &Path, command: &str, session_id: Option<&str>) -> Result<()> {
    if std::env::var_os("CRABCC_NO_REWRITE").is_some() {
        return Ok(());
    }

    // Lazy, memoised symbol predicate: opens the (read-only) Store only
    // when a bare-identifier grep/rg actually needs it.
    let db = db.to_path_buf();
    let store: RefCell<Option<Option<Store>>> = RefCell::new(None);
    let is_symbol = |name: &str| -> bool {
        let mut cell = store.borrow_mut();
        if cell.is_none() {
            *cell = Some(if db.exists() {
                Store::open(&db).ok()
            } else {
                None
            });
        }
        match cell.as_ref().and_then(|o| o.as_ref()) {
            Some(s) => matches!(s.symbol_id_by_name(name), Ok(Some(_))),
            None => false,
        }
    };

    let Some(rw) = plan(command, &is_symbol) else {
        return Ok(());
    };

    // A swap that produces an `rg` command is only safe if rg is present.
    if rw.inner.starts_with("rg ") && !rg_on_path() {
        return Ok(());
    }

    let saved = track::estimate_saved(rw.track_op, 0, 0);
    let hdr = header(&rw, saved);
    let wrapped = format!("printf '%s\\n' {}; {}", shq(&hdr), rw.inner);

    tracing::info!(
        target: "crabcc::shell::rewrite",
        rule = rw.rule,
        saved,
        session = session_id.unwrap_or(""),
        "rewrote agent shell command"
    );
    track::record(rw.track_op, command, 0, &root.to_string_lossy(), 0);

    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": wrapped }
        }
    });
    println!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never(_: &str) -> bool {
        false
    }
    fn only_store(n: &str) -> bool {
        n == "Store"
    }

    #[test]
    fn tokenize_rejects_metacharacters() {
        assert!(tokenize("grep foo | wc -l").is_none());
        assert!(tokenize("grep foo && ls").is_none());
        assert!(tokenize("grep foo $(pwd)").is_none());
        assert!(tokenize("grep foo > out.txt").is_none());
        assert!(tokenize("find . -name '*.rs' -exec rm {} ;").is_none());
    }

    #[test]
    fn tokenize_handles_quotes() {
        assert_eq!(
            tokenize("grep -rn 'foo bar' src").unwrap(),
            vec!["grep", "-rn", "foo bar", "src"]
        );
        assert!(tokenize("grep 'unbalanced").is_none());
    }

    #[test]
    fn grep_symbol_upgrade_when_indexed() {
        let rw = plan("grep -rn Store .", &only_store).unwrap();
        assert_eq!(rw.inner, "crabcc lookup refs Store");
        assert_eq!(rw.rule, "grep->crabcc-refs");
        assert_eq!(rw.track_op, "refs");
        assert!(rw.note.as_ref().unwrap().contains("rg Store"));
    }

    #[test]
    fn grep_files_only_symbol_upgrade() {
        let rw = plan("grep -rln Store", &only_store).unwrap();
        assert_eq!(rw.inner, "crabcc lookup refs Store --files-only");
    }

    #[test]
    fn grep_unknown_symbol_falls_back_to_rg() {
        let rw = plan("grep -rn Nonexistent .", &never).unwrap();
        assert_eq!(rw.inner, "rg -n Nonexistent .");
        assert_eq!(rw.track_op, "rewrite");
    }

    #[test]
    fn grep_with_path_scope_uses_rg_not_symbol() {
        // A path-scoped search must keep the scope; crabcc refs is repo-wide.
        let rw = plan("grep -rn Store src/", &only_store).unwrap();
        assert_eq!(rw.inner, "rg -n Store src/");
    }

    #[test]
    fn grep_case_insensitive_uses_rg_not_symbol() {
        let rw = plan("grep -rin Store .", &only_store).unwrap();
        assert_eq!(rw.inner, "rg -i -n Store .");
    }

    #[test]
    fn grep_literal_phrase_swaps_to_rg() {
        let rw = plan("grep -rn 'fn open' .", &never).unwrap();
        assert_eq!(rw.inner, "rg -n 'fn open' .");
    }

    #[test]
    fn grep_divergent_regex_passes_through() {
        // `+` and `?` differ between grep-BRE and ripgrep -> no rewrite.
        assert!(plan("grep -rn 'a+b' .", &never).is_none());
        assert!(plan("grep -rn 'colou?r' .", &never).is_none());
    }

    #[test]
    fn grep_fixed_string_swaps_despite_metachars_in_pattern() {
        let rw = plan("grep -rnF 'a+b' .", &never).unwrap();
        assert_eq!(rw.inner, "rg -F -n 'a+b' .");
    }

    #[test]
    fn grep_unknown_flag_passes_through() {
        assert!(plan("grep -P 'foo' .", &never).is_none());
        assert!(plan("grep --include=*.rs foo .", &never).is_none());
    }

    #[test]
    fn grep_stdin_form_passes_through() {
        // No -r and no path == reads stdin; rewriting would scan `.`.
        assert!(plan("grep foo", &never).is_none());
    }

    #[test]
    fn grep_single_file_swaps_to_rg() {
        let rw = plan("grep -n foo file.rs", &never).unwrap();
        assert_eq!(rw.inner, "rg -n foo file.rs");
    }

    #[test]
    fn rg_symbol_gets_upgraded() {
        let rw = plan("rg Store", &only_store).unwrap();
        assert_eq!(rw.inner, "crabcc lookup refs Store");
        assert_eq!(rw.rule, "rg->crabcc-refs");
    }

    #[test]
    fn rg_non_symbol_is_not_rewritten() {
        assert!(plan("rg Nonexistent", &never).is_none());
    }

    #[test]
    fn find_name_maps_to_rg_files() {
        let rw = plan("find . -name '*.rs'", &never).unwrap();
        assert_eq!(rw.inner, "rg --files -g '*.rs' .");
        assert_eq!(rw.rule, "find->rg");
    }

    #[test]
    fn find_iname_maps_to_iglob() {
        let rw = plan("find src -iname '*.RS' -type f", &never).unwrap();
        assert_eq!(rw.inner, "rg --files --iglob '*.RS' src");
    }

    #[test]
    fn find_with_exec_or_other_predicate_passes_through() {
        assert!(plan("find . -name '*.rs' -delete", &never).is_none());
        assert!(plan("find . -type d", &never).is_none());
        assert!(plan("find . -mtime -1", &never).is_none());
    }

    #[test]
    fn non_search_commands_pass_through() {
        assert!(plan("ls -la", &never).is_none());
        assert!(plan("cargo build", &never).is_none());
        assert!(plan("", &never).is_none());
    }

    #[test]
    fn header_includes_rule_and_estimate() {
        let rw = symbol_upgrade("Store", false, "grep->crabcc-refs");
        let h = header(&rw, 2000);
        assert!(h.contains("[grep->crabcc-refs]"));
        assert!(h.contains("2000 tok saved"));
        assert!(h.contains("rg Store"));
    }
}
