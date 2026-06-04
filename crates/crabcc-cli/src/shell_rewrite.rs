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
//!   * **Symbol upgrade** — `grep IDENT` / `rg IDENT` -> `crabcc lookup
//!     refs IDENT`, but *only* when IDENT is a bare identifier confirmed to be
//!     an indexed symbol and the search is repo-wide. The header
//!     discloses the symbol scope and the raw-text `rg` fallback so the
//!     model never silently loses comment/doc matches.
//!
//! Anything we do not model (perl regex, context flags, pipes, `-exec`,
//! redirects, command substitution) passes through untouched — the rule
//! is "rewrite only when certain, else do nothing". Set
//! `CRABCC_NO_REWRITE=1` to disable rewriting entirely.

use crate::rewrite_log;
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
    /// The salient argument (symbol / pattern / glob) — combined with
    /// `rule` into the suppression+log signature so one bad symbol
    /// upgrade doesn't disable unrelated rewrites.
    pub key: String,
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
pub fn plan(
    cmd: &str,
    is_symbol: &dyn Fn(&str) -> bool,
    cwd: Option<&Path>,
) -> Option<Rewrite> {
    let toks = tokenize(cmd)?;
    let (prog, rest) = toks.split_first()?;
    match prog.as_str() {
        "grep" => plan_grep(rest, is_symbol),
        "rg" => plan_rg(rest, is_symbol),
        "find" => plan_find(rest),
        "cat" => plan_cat(rest, cwd),
        _ => None,
    }
}

/// `cat <one file>` -> a cheaper, lossless reader. A `.json` file becomes
/// `jq -c .` (minified JSON, strips the indentation tokens a pretty-printed
/// file wastes). A source file becomes `crabcc read`: full content on the
/// first read, a session-cached outline *stub* on a re-read.
/// Accuracy-preserving and race-safe (mtime+hash freshness, SQLite WAL), so
/// it is never compacted — the read cache *is* the optimization. Multiple
/// files or any flag -> passthrough (plain `cat` is still compaction-worthy
/// via the post-stage chain).
fn plan_cat(args: &[String], cwd: Option<&Path>) -> Option<Rewrite> {
    let [file] = args else { return None };
    if file.starts_with('-') {
        return None;
    }
    if file.ends_with(".json") {
        return Some(Rewrite {
            inner: format!("jq -c . {}", shq(file)),
            rule: "cat-json->jq",
            key: file.clone(),
            note: Some("minified JSON (jq -c); pipe to `jq '<filter>'` to select fields".into()),
            track_op: "rewrite",
        });
    }
    if is_source_file(file) {
        // Resolve to the absolute path relative to the Bash tool's cwd so
        // the read binds to the agent's frame of reference, not crabcc's
        // process cwd. Fall back to the literal path when the file doesn't
        // exist (both `cat` and `crabcc read` would fail alike).
        let base = cwd.map(|d| d.join(file)).unwrap_or_else(|| file.into());
        let target = std::fs::canonicalize(&base)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.clone());
        return Some(Rewrite {
            inner: format!("crabcc read {}", shq(&target)),
            rule: "cat->crabcc-read",
            key: file.clone(),
            note: Some(
                "outline-aware read; a re-read in the same session returns the outline stub - add `--mode=full` for raw content"
                    .into(),
            ),
            track_op: "read",
        });
    }
    None
}

/// Source extensions where `crabcc read`'s outline stub is meaningful.
/// Data / markup formats (yaml, toml, md, csv, ...) are left to plain
/// `cat` + the compaction chain, since their outline is empty.
fn is_source_file(file: &str) -> bool {
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        ext.as_deref(),
        Some(
            "rs" | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "py"
                | "go"
                | "java"
                | "kt"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "hpp"
                | "rb"
                | "php"
                | "swift"
                | "scala"
                | "cs"
        )
    )
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
    /// `grep -H` — force filename prefix on every match line.
    with_filename: bool,
    /// `grep -h` — suppress filename prefix (even with multiple files).
    no_filename: bool,
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
                    'I' | 's' => {} // binary-skip / suppress-errors: rg default matches
                    'H' => o.with_filename = true,
                    'h' => o.no_filename = true,
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
    // Only the `find PATH... -name GLOB -type f` shape maps cleanly to
    // `rg --files -g GLOB PATH`. `-type f` is required: without it, `find`
    // can return directories that match the glob, but `rg --files` only
    // lists files, which would silently drop those directory entries.
    let mut paths = Vec::new();
    let mut glob: Option<String> = None;
    let mut iglob = false;
    let mut type_f = false;
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
                type_f = true;
                i += 2;
            }
            s if s.starts_with('-') => return None, // unknown predicate
            s => {
                paths.push(s.to_string());
                i += 1;
            }
        }
    }
    if !type_f {
        return None; // without -type f, find may match dirs; rg --files doesn't
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
        key: glob,
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
        key: pattern.to_string(),
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
    if o.with_filename {
        inner.push_str(" --with-filename");
    }
    if o.no_filename {
        inner.push_str(" --no-filename");
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
        key: pattern.to_string(),
        note: Some(
            "ripgrep: skips .gitignore'd and hidden paths (grep --no-ignore to include)".into(),
        ),
        track_op: "rewrite",
    }
}

/// Is `bin` on PATH? A swap must never emit a command the agent's
/// environment can't run, or the rewrite itself becomes the error.
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|dir| dir.join(bin).is_file()))
}

/// Programs whose output is large/unstructured enough that a compaction
/// stage (RTK / Morph) pays off. Symbol queries are already tiny, so the
/// crabcc engine rewrites that produce them are never compacted.
const COMPACTABLE: &[&str] = &[
    "cat", "gh", "git", "rg", "grep", "find", "curl", "jq", "tree",
];

/// First token of a simple (no-metacharacter) command, if it's worth
/// piping through a compaction stage.
fn compactable_program(cmd: &str) -> Option<String> {
    let toks = tokenize(cmd)?;
    let prog = toks.into_iter().next()?;
    COMPACTABLE.contains(&prog.as_str()).then_some(prog)
}

/// Morph Compact is enabled iff a key is present (privacy gate) and not
/// explicitly disabled. The network cost (~1s+ on large inputs) is opt-in
/// via the key; RTK does the bulk, local, free reduction below it.
fn morph_enabled() -> bool {
    std::env::var_os("MORPH_API_KEY").is_some() && std::env::var_os("CRABCC_NO_MORPH").is_none()
}

/// The rtk filter matching a command's output format, if rtk ships one
/// (rtk's filters are command-aware + roughly lossless, not summarisers).
fn rtk_filter_for(prog: &str) -> Option<&'static str> {
    match prog {
        "grep" | "rg" => Some("grep"),
        "find" => Some("find"),
        "cargo" => Some("cargo-test"),
        "pytest" => Some("pytest"),
        _ => None,
    }
}

/// An `rtk pipe --filter <f>` stage. **Auto-engages** (part of the default
/// chain) when `rtk` is on PATH and ships a filter for `prog` — it's local,
/// fast and free. `CRABCC_RTK_PIPE=<filter>` overrides the filter choice;
/// `CRABCC_NO_RTK` disables the stage.
fn rtk_stage(prog: &str) -> Option<String> {
    if std::env::var_os("CRABCC_NO_RTK").is_some() || !on_path("rtk") {
        return None;
    }
    let filter = std::env::var("CRABCC_RTK_PIPE")
        .ok()
        .filter(|f| !f.trim().is_empty())
        .or_else(|| rtk_filter_for(prog).map(String::from))?;
    Some(format!("rtk pipe --filter {}", shq(&filter)))
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
pub fn run(
    root: &Path,
    db: &Path,
    command: &str,
    session_id: Option<&str>,
    cwd: Option<&Path>,
) -> Result<()> {
    // Precedence: env disable-flag > .crabcc-cli.conf > built-in default.
    if std::env::var_os("CRABCC_NO_REWRITE").is_some() {
        return Ok(());
    }
    let cfg = crate::cli_config::load(root);
    if !cfg.rewrite_enabled {
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

    // ── Stage 1: engine rewrite (grep/find -> rg / lookup refs / jq).
    // Dropped if the emitted tool (rg, jq, ...) isn't on PATH — never
    // hand the agent a command its environment can't run. `crabcc` is
    // always present (we are it).
    let mut planned = plan(command, &is_symbol, cwd).filter(|rw| {
        let prog = rw.inner.split_whitespace().next().unwrap_or("");
        prog == "crabcc" || on_path(prog)
    });
    // Propagate session_id into `crabcc read` rewrites so the session-reads
    // cache in the target process sees the same session as the hook. Without
    // this the re-read outline-stub optimisation never fires in hook-driven
    // usage (the read process starts with no session and bypasses the cache).
    if let (Some(rw), Some(sid)) = (planned.as_mut(), session_id) {
        if rw.inner.starts_with("crabcc read ") && !rw.inner.contains("--session-id") {
            rw.inner = format!("{} --session-id {}", rw.inner, shq(sid));
        }
    }

    // Open the dev-debug ledger only for an actual rewrite candidate, so
    // the hot path (the vast majority of Bash commands, which don't
    // rewrite) does zero SQLite work. Best-effort; `None` => skip
    // suppression + logging, never block.
    let conn = if planned.is_some() {
        rewrite_log::open_internal()
    } else {
        None
    };

    // A prior measurement may have suppressed this (rule, key) for not
    // actually reducing tokens -> drop the engine rewrite.
    let engine = planned.filter(|rw| match &conn {
        Some(c) => !rewrite_log::is_suppressed(c, &rewrite_log::signature(rw.rule, &rw.key)),
        None => true,
    });

    // Base command + whether its output is worth compacting + the compact
    // query. Only faithful raw swaps (track_op "rewrite": rg/jq/find) are
    // compacted; crabcc's own structured outputs (refs symbol upgrades,
    // read outline stubs) are already tiny and accuracy-critical, so they
    // are never sent through RTK/Morph. Passthrough commands are compacted
    // only when compactable.
    let (base, compact_query, compact_worthy) = match &engine {
        Some(rw) => (
            rw.inner.clone(),
            (rw.rule == "grep->rg").then(|| rw.key.clone()),
            rw.track_op == "rewrite",
        ),
        None => (
            command.to_string(),
            None,
            compactable_program(command).is_some(),
        ),
    };

    // ── Stages 2-3: optional RTK filter, then Morph compact. Each is
    // opt-in (CRABCC_RTK_PIPE + rtk on PATH; MORPH_API_KEY) and a stdin
    // filter that degrades to passthrough, so the chain never loses output.
    let orig_prog = tokenize(command)
        .and_then(|t| t.into_iter().next())
        .unwrap_or_default();
    let mut stages: Vec<String> = vec![base];
    let mut chain: Vec<&str> = Vec::new();
    if compact_worthy {
        if cfg.rtk {
            if let Some(rtk) = rtk_stage(&orig_prog) {
                stages.push(rtk);
                chain.push("rtk");
            }
        }
        if cfg.morph && morph_enabled() {
            // RTK already did the bulk, free, local reduction. Only pay
            // Morph's network round-trip when the *residual* output is
            // still large enough for its query-conditioned pass to be
            // worth ~1s (cached on repeat). Keeps the default fast.
            let mut m = format!("crabcc morph compact --min-bytes {}", cfg.morph_min_bytes);
            if let Some(q) = &compact_query {
                m.push_str(" --query ");
                m.push_str(&shq(q));
            }
            stages.push(m);
            chain.push("morph");
        }
    }

    // Nothing to do: neither an engine rewrite nor any post-stage.
    if engine.is_none() && stages.len() == 1 {
        return Ok(());
    }

    let inner = stages.join(" | ");
    let saved = engine
        .as_ref()
        .map(|rw| track::estimate_saved(rw.track_op, 0, 0))
        .unwrap_or(0);
    let mut hdr = match &engine {
        Some(rw) => header(rw, saved),
        None => "## crabcc-rewrite [compact]".to_string(),
    };
    if !chain.is_empty() {
        hdr.push_str(&format!(" | +{}", chain.join("+")));
    }
    let wrapped = format!("printf '%s\\n' {}; {}", shq(&hdr), inner);

    let chain_str = chain.join("+");
    tracing::info!(
        target: "crabcc::shell::rewrite",
        rule = engine.as_ref().map(|rw| rw.rule).unwrap_or("compact"),
        saved,
        chain = chain_str.as_str(),
        session = session_id.unwrap_or(""),
        "rewrote agent shell command"
    );
    if let Some(rw) = &engine {
        track::record(rw.track_op, command, 0, &root.to_string_lossy(), 0);
    }

    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": wrapped.clone() }
        }
    });
    println!("{out}");

    // Logged after emitting so bookkeeping never delays the rewrite. We
    // store the *executed* (wrapped) command so the PostToolUse measure
    // can match it back by `tool_input.command`.
    if let (Some(c), Some(rw)) = (&conn, &engine) {
        let sig = rewrite_log::signature(rw.rule, &rw.key);
        rewrite_log::log_event(
            c,
            session_id,
            rw.rule,
            &sig,
            command,
            &wrapped,
            saved as i64,
        );
    }
    Ok(())
}

/// PostToolUse counterpart of [`run`]. Reads the hook payload on stdin,
/// and if the tool call was one of our rewrites, records the actual
/// output size so the measure/learn loop can flag + suppress rewrites
/// that did not reduce tokens. Best-effort; always exits cleanly.
pub fn run_measure() -> Result<()> {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return Ok(());
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&buf) else {
        return Ok(());
    };
    let command = v["tool_input"]["command"].as_str().unwrap_or("");
    if command.is_empty() {
        return Ok(());
    }
    // The model-visible output is the Bash tool_response. Prefer its
    // stdout field; fall back to the whole response payload.
    let resp = &v["tool_response"];
    let out_bytes = resp
        .get("stdout")
        .and_then(|s| s.as_str())
        .map(|s| s.len())
        .unwrap_or_else(|| {
            resp.as_str()
                .map(|s| s.len())
                .unwrap_or_else(|| resp.to_string().len())
        });
    let out_tokens = track::tokens_for_bytes(out_bytes) as i64;

    if let Some(conn) = rewrite_log::open_internal() {
        if let Some(verdict) = rewrite_log::measure_by_command(&conn, command, out_tokens) {
            tracing::info!(
                target: "crabcc::shell::rewrite",
                verdict,
                out_tokens,
                "measured rewritten command output"
            );
        }
    }
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

    // Each test drives the full `plan()` pipeline (tokenize -> flag parse
    // -> plan_grep/plan_rg/plan_find -> symbol_upgrade/rg_swap -> header)
    // across a family of inputs, rather than one assertion per case.

    #[test]
    fn symbol_upgrades_for_indexed_identifier() {
        // grep / rg for an indexed bare identifier, repo-wide, case-
        // sensitive, non-count -> `crabcc lookup refs` (+ --files-only),
        // with a header that discloses the rg fallback.
        let g = plan("grep -rn Store .", &only_store, None).unwrap();
        assert_eq!(g.inner, "crabcc lookup refs Store");
        assert_eq!(g.rule, "grep->crabcc-refs");
        assert_eq!(g.track_op, "refs");

        assert_eq!(
            plan("grep -rln Store", &only_store, None).unwrap().inner,
            "crabcc lookup refs Store --files-only"
        );

        let r = plan("rg Store", &only_store, None).unwrap();
        assert_eq!(r.inner, "crabcc lookup refs Store");
        assert_eq!(r.rule, "rg->crabcc-refs");

        // Header carries rule + estimate + the rg fallback note.
        let h = header(&g, 2000);
        assert!(h.contains("[grep->crabcc-refs]"), "{h}");
        assert!(h.contains("2000 tok saved"), "{h}");
        assert!(h.contains("rg Store"), "{h}");
    }

    #[test]
    fn falls_back_to_ripgrep_when_symbol_upgrade_is_unsafe() {
        // Unknown symbol, path-scoped, or case-insensitive -> faithful rg
        // swap (symbol scope/case can't be preserved by `lookup refs`).
        let unknown = plan("grep -rn Nonexistent .", &never, None).unwrap();
        assert_eq!(unknown.inner, "rg -n Nonexistent .");
        assert_eq!(unknown.track_op, "rewrite");
        assert_eq!(
            plan("grep -rn Store src/", &only_store, None).unwrap().inner,
            "rg -n Store src/"
        );
        assert_eq!(
            plan("grep -rin Store .", &only_store, None).unwrap().inner,
            "rg -i -n Store ."
        );
        // rg for a non-symbol is left alone (rg->rg is a no-op).
        assert_eq!(plan("rg Nonexistent", &never, None), None);
    }

    #[test]
    fn faithful_grep_to_ripgrep_swaps() {
        // Literal phrase, single file, and fixed-string forms all map to
        // a semantics-preserving rg invocation.
        assert_eq!(
            plan("grep -rn 'fn open' .", &never, None).unwrap().inner,
            "rg -n 'fn open' ."
        );
        assert_eq!(
            plan("grep -n foo file.rs", &never, None).unwrap().inner,
            "rg -n foo file.rs"
        );
        assert_eq!(
            plan("grep -rnF 'a+b' .", &never, None).unwrap().inner,
            "rg -F -n 'a+b' ."
        );
    }

    #[test]
    fn find_name_maps_to_ripgrep_files() {
        // -type f required: without it, find may return matching directories
        // that rg --files would silently drop — so we pass through instead.
        assert!(plan("find . -name '*.rs'", &never, None).is_none());

        assert_eq!(
            plan("find . -name '*.rs' -type f", &never, None).unwrap().inner,
            "rg --files -g '*.rs' ."
        );
        assert_eq!(
            plan("find src -iname '*.RS' -type f", &never, None)
                .unwrap()
                .inner,
            "rg --files --iglob '*.RS' src"
        );
    }

    #[test]
    fn cat_json_minifies_via_jq() {
        assert_eq!(
            plan("cat config.json", &never, None).unwrap().inner,
            "jq -c . config.json"
        );
        assert_eq!(plan("cat src/a.json", &never, None).unwrap().rule, "cat-json->jq");
        // Non-source text, flags, or multiple files are left to plain `cat`.
        assert_eq!(plan("cat README.md", &never, None), None);
        assert_eq!(plan("cat config.yaml", &never, None), None);
        assert_eq!(plan("cat -n a.json", &never, None), None);
        assert_eq!(plan("cat a.json b.json", &never, None), None);
    }

    #[test]
    fn cat_source_file_becomes_outline_aware_read() {
        // A source file -> `crabcc read` (full content first, session
        // outline stub on re-read; accuracy-preserving, never compacted).
        // Non-existent paths canonicalise to the literal fallback, so the
        // assertion is deterministic regardless of the test's cwd.
        let r = plan("cat nope_xyz.rs", &never, None).unwrap();
        assert_eq!(r.inner, "crabcc read nope_xyz.rs");
        assert_eq!(r.rule, "cat->crabcc-read");
        assert_eq!(r.track_op, "read");
        assert_eq!(
            plan("cat src/missing_zzz.go", &never, None).unwrap().rule,
            "cat->crabcc-read"
        );
    }

    #[test]
    fn unsafe_or_unknown_commands_pass_through() {
        // Shell metacharacters / pipes / substitution / redirects / -exec
        // and braces are never rewritten (tokenize bails).
        for c in [
            "grep foo | wc -l",
            "grep foo && ls",
            "grep foo $(pwd)",
            "grep foo > out.txt",
            "find . -name '*.rs' -exec rm {} ;",
            "grep 'unbalanced",
        ] {
            assert_eq!(plan(c, &never, None), None, "should pass through: {c}");
        }
        // Divergent regex (`+`/`?`), unknown/long flags, stdin-form grep,
        // non-grep/find programs, empty input, and unsupported find
        // predicates also pass through.
        for c in [
            "grep -rn 'a+b' .",
            "grep -rn 'colou?r' .",
            "grep -P 'foo' .",
            "grep --include=*.rs foo .",
            "grep foo",
            "ls -la",
            "cargo build",
            "",
            "find . -type d",
            "find . -mtime -1",
            "find . -name '*.rs' -delete",
        ] {
            assert_eq!(plan(c, &never, None), None, "should pass through: {c}");
        }
    }
}
