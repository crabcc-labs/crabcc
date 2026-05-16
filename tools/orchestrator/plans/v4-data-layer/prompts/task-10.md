# Task 10 — KG op: hot_symbols (top-N by incoming edge count)

## Context

v4.0 introduces four knowledge-graph ops over the new symbol-ID-keyed `edges`
table (`edges(id, src_symbol_id, dst_symbol_id, kind, line)`). Each op gets
its own file under `crates/crabcc-core/src/query/`.

`hot_symbols(top_n, kinds)` answers: "what are the most-depended-on symbols
in the repo?". It's a pure SQL aggregate — `GROUP BY dst_symbol_id`, count,
order, limit, then join back to `symbols` for the human-readable record.

This is the cheapest KG op of the four — one SQL statement, no BFS, no
per-row hydration loop. It's the "hot spot" report a code-reviewer would
hand to a new engineer: "these are the symbols that touch everything".

This task assumes Task 2 (Store API for v4) has landed a
`pub fn conn(&self) -> &rusqlite::Connection` accessor on `Store`. The
implementation below calls `store.conn()`.

This task ONLY creates `crates/crabcc-core/src/query/hot_symbols.rs`. Do
NOT touch `crates/crabcc-core/src/query/mod.rs` — Task 8 owns the
restructure and Task 12 wires `pub mod hot_symbols;` into the module tree
when it integrates the CLI dispatcher.

## What to change

### Create `crates/crabcc-core/src/query/hot_symbols.rs`

Create a new file with this exact content:

```rust
//! KG op: hot-symbols. Top-N most-depended-on symbols in the repo,
//! ranked by incoming edge count. One SQL statement with a GROUP BY
//! and a LIMIT — the entire op is a single index scan plus an order.
//!
//! v4 edge schema:
//!   edges(id, src_symbol_id, dst_symbol_id, kind, line)
//!   indexed by (dst_symbol_id, kind) — the GROUP BY picks up the index.
//!
//! `kinds` filters which edge kinds count toward in-degree. Empty slice
//! means "all kinds". Typical CLI usage:
//!   - `&["call"]` for "most-called functions"
//!   - `&["ref"]` for "most-referenced types"
//!   - `&[]` for raw in-degree across everything

use crate::store::Store;
use crate::types::{Symbol, SymbolKind};
use anyhow::Result;
use rusqlite::{params, params_from_iter};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HotSymbol {
    pub symbol: Symbol,
    /// Number of incoming edges pointing at this symbol. With `kinds`
    /// filtering, only edges matching one of the kinds are counted.
    pub in_degree: usize,
}

pub fn hot_symbols(store: &Store, top_n: usize, kinds: &[&str]) -> Result<Vec<HotSymbol>> {
    if top_n == 0 {
        return Ok(Vec::new());
    }
    let conn = store.conn();

    // Single SQL: aggregate in-degree by dst_symbol_id, then join through
    // to symbols+files for the record. Ordering ties are broken by
    // dst_symbol_id ASC for determinism (so identical fixtures produce
    // identical output — matters for the integration test fingerprint).
    let (sql, kind_count) = if kinds.is_empty() {
        (
            "SELECT s.name, s.kind, s.signature, f.path, s.line_start, s.line_end, \
                    s.visibility, deg.in_degree \
             FROM (SELECT dst_symbol_id, COUNT(*) AS in_degree \
                   FROM edges GROUP BY dst_symbol_id) AS deg \
             JOIN symbols s ON s.id = deg.dst_symbol_id \
             JOIN files f ON f.id = s.file_id \
             ORDER BY deg.in_degree DESC, deg.dst_symbol_id ASC \
             LIMIT ?1"
                .to_string(),
            0,
        )
    } else {
        let placeholders: Vec<String> =
            (0..kinds.len()).map(|i| format!("?{}", i + 2)).collect();
        (
            format!(
                "SELECT s.name, s.kind, s.signature, f.path, s.line_start, s.line_end, \
                        s.visibility, deg.in_degree \
                 FROM (SELECT dst_symbol_id, COUNT(*) AS in_degree \
                       FROM edges WHERE kind IN ({}) \
                       GROUP BY dst_symbol_id) AS deg \
                 JOIN symbols s ON s.id = deg.dst_symbol_id \
                 JOIN files f ON f.id = s.file_id \
                 ORDER BY deg.in_degree DESC, deg.dst_symbol_id ASC \
                 LIMIT ?1",
                placeholders.join(",")
            ),
            kinds.len(),
        )
    };

    let mut stmt = conn.prepare(&sql)?;

    let rows = if kind_count == 0 {
        stmt.query_map(params![top_n as i64], |row| {
            Ok(HotSymbol {
                symbol: Symbol {
                    name: row.get(0)?,
                    kind: kind_from_str(&row.get::<_, String>(1)?),
                    signature: row.get(2)?,
                    parent: None,
                    file: row.get(3)?,
                    line_start: row.get(4)?,
                    line_end: row.get(5)?,
                    visibility: row.get(6)?,
                },
                in_degree: row.get::<_, i64>(7)? as usize,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(1 + kind_count);
        bound.push(Box::new(top_n as i64));
        for k in kinds {
            bound.push(Box::new(k.to_string()));
        }
        stmt.query_map(
            params_from_iter(bound.iter().map(|b| b.as_ref())),
            |row| {
                Ok(HotSymbol {
                    symbol: Symbol {
                        name: row.get(0)?,
                        kind: kind_from_str(&row.get::<_, String>(1)?),
                        signature: row.get(2)?,
                        parent: None,
                        file: row.get(3)?,
                        line_start: row.get(4)?,
                        line_end: row.get(5)?,
                        visibility: row.get(6)?,
                    },
                    in_degree: row.get::<_, i64>(7)? as usize,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

fn kind_from_str(s: &str) -> SymbolKind {
    match s {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "const" => SymbolKind::Const,
        "var" => SymbolKind::Var,
        "type" => SymbolKind::Type,
        "macro" => SymbolKind::Macro,
        _ => SymbolKind::Function,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// Fixture: 4 symbols (a, b, c, d). Incoming edges:
    ///   a ← b (call), a ← c (call), a ← d (ref)   → in_degree 3
    ///   b ← c (call), b ← d (call)                → in_degree 2
    ///   c ← d (call)                              → in_degree 1
    ///   d                                          → in_degree 0 (no row)
    /// Bypasses the extractor — we want to verify the aggregate, not the parser.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        let conn = store.conn();
        conn.execute(
            "INSERT INTO files(path, sha256, mtime, lang, indexed_at) \
             VALUES ('a.rs','x',0,'rust',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(id, file_id, name, kind, line_start, line_end) VALUES \
             (1, 1, 'a', 'function', 1, 5), \
             (2, 1, 'b', 'function', 7, 11), \
             (3, 1, 'c', 'function', 13, 17), \
             (4, 1, 'd', 'function', 19, 23)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line) VALUES \
             (2, 1, 'call', 8), \
             (3, 1, 'call', 14), \
             (4, 1, 'ref',  20), \
             (3, 2, 'call', 14), \
             (4, 2, 'call', 21), \
             (4, 3, 'call', 22)",
            [],
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn hot_symbols_ranks_by_in_degree() {
        let (_dir, store) = fixture();
        let r = hot_symbols(&store, 3, &[]).unwrap();
        assert_eq!(r.len(), 3, "expected 3 hottest: {:?}", r);
        // a has 3 incoming, b has 2, c has 1 → that order.
        assert_eq!(r[0].symbol.name, "a");
        assert_eq!(r[0].in_degree, 3);
        assert_eq!(r[1].symbol.name, "b");
        assert_eq!(r[1].in_degree, 2);
        assert_eq!(r[2].symbol.name, "c");
        assert_eq!(r[2].in_degree, 1);
    }

    #[test]
    fn hot_symbols_filters_by_kind() {
        let (_dir, store) = fixture();
        // Only 'call' edges: a has 2 (b, c), b has 2 (c, d), c has 1 (d).
        // The ref edge from d → a is filtered out, dropping a's count.
        let r = hot_symbols(&store, 5, &["call"]).unwrap();
        // a and b both have 2 calls — tie broken by dst_symbol_id ASC
        // (a.id=1, b.id=2), so a is still first.
        assert!(r.len() >= 2);
        assert_eq!(r[0].symbol.name, "a");
        assert_eq!(r[0].in_degree, 2);
        assert_eq!(r[1].symbol.name, "b");
        assert_eq!(r[1].in_degree, 2);
    }

    #[test]
    fn hot_symbols_top_n_zero_returns_empty() {
        let (_dir, store) = fixture();
        let r = hot_symbols(&store, 0, &[]).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn hot_symbols_top_n_caps_results() {
        let (_dir, store) = fixture();
        let r = hot_symbols(&store, 1, &[]).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].symbol.name, "a");
    }
}
```

## Constraints

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(query): hot_symbols — top-N by incoming edge count
