//! `crabcc edit FILE#SYMBOL` — AST-targeted edit.
//!
//! Resolve a symbol to its exact line span via the index and rewrite ONLY
//! that span, so an agent sends ~10 lines (one fn / struct / impl) instead of
//! re-emitting a whole file. The new span is either:
//!
//! - spliced **verbatim** (`--replace`, deterministic, no network), or
//! - merged from a **lazy edit** via Morph Fast Apply (`--lazy`, gated on
//!   `MORPH_API_KEY`; the merge *is* the operation, so it errors without a key
//!   rather than silently passing through).
//!
//! Default (`auto`) picks lazy when `MORPH_API_KEY` is set, else replace.
//!
//! Without `--write` it previews the rewritten span (cheap — just the symbol);
//! with `--write` it splices the file in place and prints a summary.

use anyhow::{anyhow, bail, Context, Result};
use crabcc_core::store::Store;
use crabcc_core::types::Symbol;
use serde_json::{json, Value};
use std::io::Read as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Splice the update verbatim as the symbol's new body.
    Replace,
    /// Merge a lazy edit into the symbol via Morph Fast Apply.
    Lazy,
    /// Lazy when `MORPH_API_KEY` is set, else replace.
    Auto,
}

impl EditMode {
    pub fn from_flags(replace: bool, lazy: bool) -> Result<Self> {
        match (replace, lazy) {
            (true, true) => bail!("--replace and --lazy are mutually exclusive"),
            (true, false) => Ok(Self::Replace),
            (false, true) => Ok(Self::Lazy),
            (false, false) => Ok(Self::Auto),
        }
    }
}

/// Thin CLI entry: parse `FILE#SYMBOL`, resolve the span, compute the new
/// body, then preview or write. Compute lives in [`compute`] so it stays
/// testable without a process/network.
pub fn run(
    root: &Path,
    store: &Store,
    target: &str,
    update_arg: Option<String>,
    replace: bool,
    lazy: bool,
    write: bool,
) -> Result<()> {
    let mode = EditMode::from_flags(replace, lazy)?;
    let (file, selector) = parse_target(target)?;
    let update = match update_arg {
        Some(u) => u,
        None => read_stdin()?,
    };
    let sym = resolve_symbol(store, &file, &selector)?;

    let abs: PathBuf = if Path::new(&file).is_absolute() {
        PathBuf::from(&file)
    } else {
        root.join(&file)
    };
    let src = std::fs::read_to_string(&abs).with_context(|| format!("read {}", abs.display()))?;

    let (new_content, new_span, used_mode) = compute(&src, &sym, &update, mode)?;

    let payload = if write {
        std::fs::write(&abs, &new_content).with_context(|| format!("write {}", abs.display()))?;
        // Refresh the index for the touched file: the write shifted line
        // numbers, so without this a later edit in the same file would resolve
        // against stale `line_start`/`line_end` and splice into the wrong span.
        let reindexed = reindex_file(store, &file, &abs).is_ok();
        json!({
            "file": file,
            "symbol": selector,
            "kind": format!("{:?}", sym.kind),
            "span": format!("{}-{}", sym.line_start, sym.line_end),
            "mode": used_mode,
            "written": true,
            "bytes": new_content.len(),
            "reindexed": reindexed,
        })
    } else {
        json!({
            "file": file,
            "symbol": selector,
            "kind": format!("{:?}", sym.kind),
            "span": format!("{}-{}", sym.line_start, sym.line_end),
            "mode": used_mode,
            "written": false,
            "preview": new_span,
            "note": "previewed the rewritten symbol; pass --write to splice it into the file",
        })
    };

    crabcc_core::track::record(
        "edit",
        target,
        1,
        &repo_label(root),
        payload.to_string().len(),
    );
    println!("{payload}");
    Ok(())
}

/// Split `target` into `(file, selector)` on the last `#`. The selector is a
/// symbol name, optionally `Parent::name` to disambiguate.
fn parse_target(target: &str) -> Result<(String, String)> {
    let (file, sel) = target.rsplit_once('#').ok_or_else(|| {
        anyhow!("target must be FILE#SYMBOL (e.g. src/store.rs#Store::open), got {target:?}")
    })?;
    if file.is_empty() || sel.is_empty() {
        bail!("target must be FILE#SYMBOL with both parts non-empty, got {target:?}");
    }
    Ok((file.to_string(), sel.to_string()))
}

/// Resolve `selector` (`name` or `Parent::name`) to a unique [`Symbol`] in
/// `file`. Errors with the candidate list when missing or ambiguous.
fn resolve_symbol(store: &Store, file: &str, selector: &str) -> Result<Symbol> {
    let syms = store.symbols_in_file(file)?;
    if syms.is_empty() {
        bail!("no indexed symbols in {file:?} (wrong path, or run `crabcc index`?)");
    }
    let (parent_sel, name) = match selector.rsplit_once("::") {
        Some((p, n)) => (Some(p), n),
        None => (None, selector),
    };
    let matches: Vec<&Symbol> = syms
        .iter()
        .filter(|s| s.name == name && parent_sel.is_none_or(|p| s.parent.as_deref() == Some(p)))
        .collect();
    match matches.as_slice() {
        [] => bail!(
            "symbol {selector:?} not found in {file}; candidates: {}",
            candidate_list(&syms)
        ),
        [one] => Ok((*one).clone()),
        many => bail!(
            "symbol {selector:?} is ambiguous in {file} ({} matches) - qualify as Parent::name. candidates: {}",
            many.len(),
            candidate_list(&syms)
        ),
    }
}

fn candidate_list(syms: &[Symbol]) -> String {
    syms.iter()
        .map(|s| match &s.parent {
            Some(p) => format!("{}::{} [{}-{}]", p, s.name, s.line_start, s.line_end),
            None => format!("{} [{}-{}]", s.name, s.line_start, s.line_end),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pure core: produce `(new_file, new_span, used_mode)` from the original
/// source, the resolved symbol, the caller's update, and the mode.
pub fn compute(
    src: &str,
    sym: &Symbol,
    update: &str,
    mode: EditMode,
) -> Result<(String, String, &'static str)> {
    let (resolved_mode, used) = match mode {
        EditMode::Replace => (EditMode::Replace, "replace"),
        EditMode::Lazy => (EditMode::Lazy, "lazy"),
        EditMode::Auto if crate::morph::api_key().is_some() => (EditMode::Lazy, "lazy"),
        EditMode::Auto => (EditMode::Replace, "replace"),
    };

    let old_span = extract_span(src, sym.line_start, sym.line_end)?;
    let new_span = match resolved_mode {
        EditMode::Replace | EditMode::Auto => update.to_string(),
        EditMode::Lazy => {
            let instruction = format!(
                "Apply the update to the {:?} `{}`. Return only the rewritten symbol.",
                sym.kind, sym.name
            );
            crate::morph::apply(&instruction, &old_span, update)?
        }
    };
    let new_content = splice_span(src, sym.line_start, sym.line_end, &new_span)?;
    Ok((new_content, new_span, used))
}

/// Extract the 1-indexed inclusive line range `[start, end]` from `src`.
fn extract_span(src: &str, start: u32, end: u32) -> Result<String> {
    let lines: Vec<&str> = src.split('\n').collect();
    let (s, e) = span_bounds(lines.len(), start, end)?;
    Ok(lines[s..e].join("\n"))
}

/// Replace the 1-indexed inclusive line range `[start, end]` with `new_span`,
/// preserving the rest of the file byte-for-byte (including the trailing
/// newline, which `split('\n')` represents as a final empty element).
pub fn splice_span(src: &str, start: u32, end: u32, new_span: &str) -> Result<String> {
    let lines: Vec<&str> = src.split('\n').collect();
    let (s, e) = span_bounds(lines.len(), start, end)?;
    // `split('\n')` keeps each line's trailing `\r`, so the unchanged lines are
    // preserved byte-for-byte (CRLF stays CRLF after `join("\n")`). The only
    // gap is the update, which arrives as LF: give its lines the file's `\r`
    // when the file is CRLF so we don't inject mixed endings.
    let crlf = src.contains("\r\n");
    // Drop a single trailing newline on the update so we don't add a blank line.
    let new_trimmed = new_span.strip_suffix('\n').unwrap_or(new_span);
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    out.extend(lines[..s].iter().map(|l| l.to_string()));
    for line in new_trimmed.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push(if crlf {
            format!("{line}\r")
        } else {
            line.to_string()
        });
    }
    out.extend(lines[e..].iter().map(|l| l.to_string()));
    Ok(out.join("\n"))
}

/// Convert a 1-indexed inclusive `[start, end]` line range into a
/// 0-indexed half-open `[s, e)` slice range, validating against `n` lines.
fn span_bounds(n: usize, start: u32, end: u32) -> Result<(usize, usize)> {
    if start < 1 || end < start {
        bail!("invalid symbol span {start}-{end}");
    }
    let s = start as usize - 1;
    let e = end as usize;
    if e > n {
        bail!(
            "symbol span {start}-{end} runs past end of file ({n} lines) - index may be stale; re-run `crabcc index`"
        );
    }
    Ok((s, e))
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read update from stdin")?;
    if buf.trim().is_empty() {
        bail!("no update provided (pass --update or pipe the new symbol body on stdin)");
    }
    Ok(buf)
}

/// Re-extract `rel` from disk and replace its index rows, so a later edit in
/// the same file resolves against the new spans. Mirrors the per-file path of
/// `crabcc_core::index::build_index`; best-effort (an unsupported language,
/// unreadable bytes, or a parse error leaves the file written but the index
/// untouched, exactly as a full index would skip it).
fn reindex_file(store: &Store, rel: &str, abs: &Path) -> Result<()> {
    use crabcc_core::{extract, hash};
    let Some(lang) = extract::detect_lang(abs) else {
        return Ok(());
    };
    let bytes = std::fs::read(abs).with_context(|| format!("reindex read {}", abs.display()))?;
    let Ok(src) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    let Ok((symbols, edges)) = extract::extract_file_with_edges(rel, src, lang) else {
        return Ok(());
    };
    let sha = hash::sha256_hex(&bytes);
    let mtime = std::fs::metadata(abs)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let file_id = store.upsert_file(rel, &sha, mtime, lang)?;
    store.replace_symbols(file_id, &symbols)?;
    store.replace_edges(file_id, &edges)?;
    Ok(())
}

fn repo_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_else(|| root.to_string_lossy())
        .into_owned()
}

/// Re-exported for callers that want the raw JSON shape (none yet; keeps the
/// surface symmetric with `read`).
#[allow(dead_code)]
pub type EditPayload = Value;

#[cfg(test)]
mod tests {
    use super::*;
    use crabcc_core::types::{Symbol, SymbolKind};

    fn sym(name: &str, start: u32, end: u32, parent: Option<&str>) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            signature: None,
            parent: parent.map(|p| p.into()),
            file: "x.rs".into(),
            line_start: start,
            line_end: end,
            visibility: None,
        }
    }

    #[test]
    fn parse_target_splits_on_last_hash() {
        assert_eq!(
            parse_target("src/a.rs#Store::open").unwrap(),
            ("src/a.rs".into(), "Store::open".into())
        );
        assert!(parse_target("no-hash").is_err());
        assert!(parse_target("#sym").is_err());
        assert!(parse_target("file#").is_err());
    }

    #[test]
    fn splice_replaces_only_the_span_and_keeps_trailing_newline() {
        let src = "fn a() {}\nfn b() {\n    old();\n}\nfn c() {}\n";
        // `fn b` spans lines 2-4 inclusive.
        let out = splice_span(src, 2, 4, "fn b() {\n    new();\n}").unwrap();
        assert_eq!(out, "fn a() {}\nfn b() {\n    new();\n}\nfn c() {}\n");
    }

    #[test]
    fn splice_handles_first_and_last_symbol() {
        let src = "fn a() {}\nfn b() {}\n";
        assert_eq!(
            splice_span(src, 1, 1, "fn a2() {}").unwrap(),
            "fn a2() {}\nfn b() {}\n"
        );
        // Last real line is 2 ("fn b"), line 3 is the empty trailing element.
        assert_eq!(
            splice_span(src, 2, 2, "fn b2() {}").unwrap(),
            "fn a() {}\nfn b2() {}\n"
        );
    }

    #[test]
    fn splice_rejects_stale_out_of_range_span() {
        let src = "one\ntwo\n";
        assert!(splice_span(src, 5, 6, "x").is_err());
        assert!(splice_span(src, 0, 1, "x").is_err());
    }

    #[test]
    fn compute_replace_mode_splices_verbatim() {
        let src = "fn a() {}\nfn target() {\n    body();\n}\nfn z() {}\n";
        let s = sym("target", 2, 4, None);
        let (content, span, mode) = compute(
            src,
            &s,
            "fn target() {\n    new_body();\n}",
            EditMode::Replace,
        )
        .unwrap();
        assert_eq!(mode, "replace");
        assert_eq!(span, "fn target() {\n    new_body();\n}");
        assert_eq!(
            content,
            "fn a() {}\nfn target() {\n    new_body();\n}\nfn z() {}\n"
        );
    }

    #[test]
    fn extract_span_returns_symbol_lines() {
        let src = "a\nb\nc\nd\n";
        assert_eq!(extract_span(src, 2, 3).unwrap(), "b\nc");
    }

    #[test]
    fn splice_preserves_crlf_endings_including_the_new_span() {
        // CRLF file; the update arrives LF-only. Unchanged lines must keep
        // CRLF and the spliced span must adopt CRLF too (no mixed endings).
        let src = "fn a() {}\r\nfn b() {\r\n    old();\r\n}\r\nfn c() {}\r\n";
        let out = splice_span(src, 2, 4, "fn b() {\n    new();\n}").unwrap();
        assert_eq!(
            out,
            "fn a() {}\r\nfn b() {\r\n    new();\r\n}\r\nfn c() {}\r\n"
        );
        // After removing every CRLF pair, no lone LF should remain.
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "bare LF leaked in: {out:?}"
        );
    }
}
