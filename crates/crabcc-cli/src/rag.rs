//! `crabcc rag` — symbol-aware code retrieval (RAG) over the indexed
//! symbols, built on the existing crabcc-memory `Palace` (FTS5 BM25 +
//! sqlite-vec ANN, fused via RRF).
//!
//! Why this and not `memory mine project`: that miner stores **one drawer
//! per file**, so a query returns whole files and the embedding of a 600-
//! line file is diluted. `rag build` chunks at **symbol** granularity
//! (one drawer per fn/struct/impl, body = signature + source span), which
//! is the right unit for "find the snippet relevant to this prompt and
//! inject only that".
//!
//! Accuracy note: vector RAG is *fuzzy* retrieval. It complements — never
//! replaces — the precise `lookup sym/refs/callers` surface, so this is an
//! explicit opt-in command, never a silent rewrite. Without
//! `--features memory-embed` the Palace falls back to lexical BM25 (still
//! useful, relevance-ranked); with it, you get semantic MiniLM hybrid.

use anyhow::Result;
use crabcc_core::store::Store;
use crabcc_core::track;
use crabcc_memory::{DeleteSel, Palace, QueryResult};
use serde::Serialize;
use std::path::Path;

/// Drawer wing the code chunks live under — keeps them isolated from
/// `proj`/`session`/note drawers so queries can filter to code only.
const WING: &str = "code";

/// Don't embed a symbol whose source span exceeds this (bytes). A giant
/// generated `impl` dilutes the vector and bloats FTS; the symbol index
/// already covers "where is it" for those.
const MAX_CHUNK_BYTES: usize = 6_000;

/// Build the chunk body for one symbol: a header line (so lexical search
/// matches the name/kind/path) + the signature + the source span.
fn chunk_body(sym: &crabcc_core::types::Symbol, source: &str) -> Option<String> {
    let start = sym.line_start.saturating_sub(1) as usize;
    let end = (sym.line_end as usize).max(sym.line_start as usize);
    let span: String = source
        .lines()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<Vec<_>>()
        .join("\n");
    if span.trim().is_empty() {
        return None;
    }
    let header = format!("{}:{} [{:?}]", sym.file, sym.name, sym.kind);
    let sig = sym.signature.as_deref().unwrap_or("");
    let body = format!("{header}\n{sig}\n{span}");
    Some(body.chars().take(MAX_CHUNK_BYTES).collect())
}

/// `code:<file>#<name>@<line>` — round-trips back to file/name/line for
/// the query output. `#`/`@` don't occur in repo paths or Rust idents.
fn source_id(sym: &crabcc_core::types::Symbol) -> String {
    format!("code:{}#{}@{}", sym.file, sym.name, sym.line_start)
}

#[derive(Serialize)]
struct BuildReport {
    files: usize,
    symbols: usize,
    chunked: usize,
    skipped: usize,
    inserted: usize,
}

/// `crabcc rag build [--rebuild]`: chunk every indexed symbol into the
/// `code` wing. Idempotent (Palace dedups on `(source_id, sha256)`);
/// `--rebuild` clears the wing first so renamed/deleted symbols don't
/// leave stale chunks behind.
pub fn run_build(root: &Path, store: &Store, rebuild: bool) -> Result<()> {
    let palace = Palace::open(root)?;
    if rebuild {
        // Far-future `before` deletes every existing code drawer.
        palace.forget(&DeleteSel::BeforeInWing {
            wing: WING.into(),
            before: i64::MAX,
        })?;
    }

    let before = palace.count()?;
    let mut rep = BuildReport {
        files: 0,
        symbols: 0,
        chunked: 0,
        skipped: 0,
        inserted: 0,
    };

    for (file, _lang) in store.list_files()? {
        rep.files += 1;
        let abs = root.join(&file);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            continue; // file gone / binary / unreadable -> skip, never fail
        };
        for sym in store.symbols_in_file(&file)? {
            rep.symbols += 1;
            let Some(body) = chunk_body(&sym, &source) else {
                rep.skipped += 1;
                continue;
            };
            palace.remember_in_session(WING, Some(&file), &source_id(&sym), &body, None)?;
            rep.chunked += 1;
        }
    }

    rep.inserted = palace.count()?.saturating_sub(before);
    println!("{}", serde_json::to_string(&rep)?);
    Ok(())
}

#[derive(Serialize)]
struct CodeHit {
    file: String,
    symbol: String,
    line: u32,
    score: f32,
    snippet: String,
}

#[derive(Serialize)]
struct QueryOut {
    query: String,
    hits: Vec<CodeHit>,
}

/// Parse `code:<file>#<name>@<line>` back into (file, name, line).
fn parse_source_id(id: &str) -> (String, String, u32) {
    let rest = id.strip_prefix("code:").unwrap_or(id);
    let (file, tail) = rest.split_once('#').unwrap_or((rest, ""));
    let (name, line) = tail.split_once('@').unwrap_or((tail, "0"));
    (
        file.to_string(),
        name.to_string(),
        line.parse().unwrap_or(0),
    )
}

/// First `n` non-empty lines of a chunk body, skipping the synthetic
/// header line, as an injection-ready preview.
fn snippet(body: &str, n: usize) -> String {
    body.lines().skip(1).take(n).collect::<Vec<_>>().join("\n")
}

fn to_out(query: &str, res: QueryResult) -> QueryOut {
    let hits = res
        .hits
        .into_iter()
        .map(|h| {
            let (file, symbol, line) = parse_source_id(&h.source_id);
            CodeHit {
                file,
                symbol,
                line,
                score: h.score,
                snippet: snippet(&h.body, 12),
            }
        })
        .collect();
    QueryOut {
        query: query.to_string(),
        hits,
    }
}

/// `crabcc rag query <q> [--limit N]`: hybrid (or lexical, sans
/// `memory-embed`) search scoped to the `code` wing. Returns the top-K
/// relevant snippets for prompt injection.
pub fn run_query(root: &Path, query: &str, limit: usize) -> Result<()> {
    let palace = Palace::open(root)?;
    let res = palace.search_filtered(query, limit, Some(WING), None)?;
    let out = to_out(query, res);
    let body = serde_json::to_string(&out)?;
    track::record("rag", query, out.hits.len(), &repo_label(root), body.len());
    println!("{body}");
    Ok(())
}

fn repo_label(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabcc_core::types::{Symbol, SymbolKind};

    fn sym(name: &str, file: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            name: name.into(),
            kind: SymbolKind::Function,
            signature: Some(format!("fn {name}()")),
            parent: None,
            file: file.into(),
            line_start: start,
            line_end: end,
            visibility: None,
        }
    }

    #[test]
    fn chunk_body_slices_the_span_and_skips_empty() {
        let src = "line1\nfn foo() {\n  work();\n}\nline5\n";
        let body = chunk_body(&sym("foo", "a.rs", 2, 4), src).unwrap();
        assert!(body.contains("a.rs:foo"), "header present: {body}");
        assert!(body.contains("fn foo()"));
        assert!(body.contains("work();"));
        assert!(!body.contains("line5"), "span must not overrun line_end");
        // A span that lands on blank lines is dropped.
        assert!(chunk_body(&sym("blank", "a.rs", 99, 99), src).is_none());
    }

    #[test]
    fn source_id_round_trips() {
        let s = sym("Store", "crates/core/src/store.rs", 42, 88);
        let id = source_id(&s);
        assert_eq!(id, "code:crates/core/src/store.rs#Store@42");
        let (file, name, line) = parse_source_id(&id);
        assert_eq!(file, "crates/core/src/store.rs");
        assert_eq!(name, "Store");
        assert_eq!(line, 42);
    }

    #[test]
    fn snippet_skips_header_line() {
        let body = "HEADER a.rs:foo\nfn foo()\nbody1\nbody2";
        assert_eq!(snippet(body, 2), "fn foo()\nbody1");
    }
}
