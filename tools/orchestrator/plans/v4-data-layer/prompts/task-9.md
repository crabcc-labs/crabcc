# Task 9 — KG op: why (bidirectional BFS, path reconstruction)

## Context

v4.0 introduces four knowledge-graph ops over the new symbol-ID-keyed `edges`
table (`edges(id, src_symbol_id, dst_symbol_id, kind, line)`). Each op gets
its own file under `crates/crabcc-core/src/query/`.

`why(src, dst, max_depth)` answers: "is there a path from src to dst, and if
so, what does it look like?". The algorithm is a bidirectional BFS — expand
the forward frontier from `src` (following `WHERE src_symbol_id = ?`) and the
reverse frontier from `dst` (following `WHERE dst_symbol_id = ?`) in
alternation. When the two frontiers meet at some node `m`, the meeting node
is used to reconstruct the chain `src -> ... -> m -> ... -> dst`. Returns
`None` if the frontiers never meet within `max_depth` total hops.

Bidirectional BFS is the right shape for this query because forward-only or
reverse-only BFS in a code-graph blows up fast (fan-out at depth 5 is
millions on a 13k-file repo); meeting in the middle keeps the visited set
bounded by roughly the square root of either single direction's frontier.

This task assumes Task 2 (Store API for v4) has landed a
`pub fn conn(&self) -> &rusqlite::Connection` accessor on `Store`. The
implementation below calls `store.conn()`.

This task ONLY creates `crates/crabcc-core/src/query/why.rs`. Do not touch
`crates/crabcc-core/src/query/mod.rs` — Task 8 owns the restructure and
Task 12 wires `pub mod why;` into the module tree when it integrates the
CLI dispatcher.

## What to change

### Create `crates/crabcc-core/src/query/why.rs`

Create a new file with this exact content:

```rust
//! KG op: why-path. Given two symbol ids `src` and `dst`, find the
//! shortest chain `src -> ... -> dst` in the edges graph (any kind).
//! Implementation is a bidirectional BFS that meets in the middle.
//!
//! v4 edge schema:
//!   edges(id, src_symbol_id, dst_symbol_id, kind, line)
//!   indexed by (src_symbol_id) and (dst_symbol_id).
//!
//! Returns `None` when the two frontiers don't meet within `max_depth`
//! total hops. `max_depth` is the cap on the sum of forward+reverse hops,
//! so `max_depth=4` permits chains up to 4 edges long.

use crate::store::Store;
use crate::types::{Symbol, SymbolKind};
use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Serialize)]
pub struct Path {
    /// Symbols along the path from `src` to `dst`, inclusive. Length 2
    /// means a direct edge; length 3 means one intermediate; etc.
    pub nodes: Vec<Symbol>,
    /// One entry per edge in `nodes`, length = `nodes.len() - 1`. Each
    /// is `(src_symbol_id, dst_symbol_id, kind, line)`. The kind and
    /// line are pulled from the actual chosen edge; if multiple edges
    /// exist between the same pair, we surface the first one we found
    /// during the BFS (no guarantee of which kind wins on ties).
    pub edges: Vec<(i64, i64, String, i64)>,
}

/// Bidirectional BFS for a path from `src` to `dst`. Returns `None` if
/// `max_depth` is 0 (no hops allowed) and `src != dst`, or if the
/// frontiers don't meet within the budget.
pub fn why(store: &Store, src: i64, dst: i64, max_depth: usize) -> Result<Option<Path>> {
    if src == dst {
        let nodes = hydrate_symbols(store, vec![src])?;
        return Ok(Some(Path {
            nodes,
            edges: Vec::new(),
        }));
    }
    if max_depth == 0 {
        return Ok(None);
    }

    let conn = store.conn();
    // parent_fwd[node] = the node we came from while expanding from `src`.
    // parent_rev[node] = the node we came from while expanding from `dst`.
    // edge_fwd[(src_id, dst_id)] = (kind, line) of the chosen edge.
    let mut parent_fwd: HashMap<i64, i64> = HashMap::new();
    let mut parent_rev: HashMap<i64, i64> = HashMap::new();
    let mut edge_meta: HashMap<(i64, i64), (String, i64)> = HashMap::new();
    let mut seen_fwd: HashSet<i64> = HashSet::new();
    let mut seen_rev: HashSet<i64> = HashSet::new();
    seen_fwd.insert(src);
    seen_rev.insert(dst);

    let mut frontier_fwd: VecDeque<i64> = VecDeque::from([src]);
    let mut frontier_rev: VecDeque<i64> = VecDeque::from([dst]);

    let mut fwd_stmt = conn.prepare(
        "SELECT dst_symbol_id, kind, line FROM edges WHERE src_symbol_id = ?1",
    )?;
    let mut rev_stmt = conn.prepare(
        "SELECT src_symbol_id, kind, line FROM edges WHERE dst_symbol_id = ?1",
    )?;

    // Alternate one BFS level on each side until they meet or budget runs out.
    let mut hops_done: usize = 0;
    while hops_done < max_depth {
        hops_done += 1;

        // Expand forward frontier one hop.
        let mut next_fwd: VecDeque<i64> = VecDeque::new();
        for cur in frontier_fwd.drain(..) {
            let rows = fwd_stmt.query_map(params![cur], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for r in rows {
                let (nxt, kind, line) = r?;
                if seen_fwd.insert(nxt) {
                    parent_fwd.insert(nxt, cur);
                    edge_meta.entry((cur, nxt)).or_insert((kind, line));
                    if seen_rev.contains(&nxt) {
                        // Frontiers met at `nxt` — reconstruct.
                        return Ok(Some(build_path(
                            store, src, dst, nxt, &parent_fwd, &parent_rev, &edge_meta,
                        )?));
                    }
                    next_fwd.push_back(nxt);
                }
            }
        }
        frontier_fwd = next_fwd;
        if frontier_fwd.is_empty() {
            // Forward search exhausted with no meet — no path possible.
            return Ok(None);
        }

        if hops_done >= max_depth {
            break;
        }
        hops_done += 1;

        // Expand reverse frontier one hop.
        let mut next_rev: VecDeque<i64> = VecDeque::new();
        for cur in frontier_rev.drain(..) {
            let rows = rev_stmt.query_map(params![cur], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for r in rows {
                let (prv, kind, line) = r?;
                if seen_rev.insert(prv) {
                    parent_rev.insert(prv, cur);
                    edge_meta.entry((prv, cur)).or_insert((kind, line));
                    if seen_fwd.contains(&prv) {
                        return Ok(Some(build_path(
                            store, src, dst, prv, &parent_fwd, &parent_rev, &edge_meta,
                        )?));
                    }
                    next_rev.push_back(prv);
                }
            }
        }
        frontier_rev = next_rev;
        if frontier_rev.is_empty() {
            return Ok(None);
        }
    }

    Ok(None)
}

/// Walk `parent_fwd` from `meet` back to `src`, then `parent_rev` from
/// `meet` forward to `dst`, stitch them, and hydrate symbol records.
fn build_path(
    store: &Store,
    src: i64,
    dst: i64,
    meet: i64,
    parent_fwd: &HashMap<i64, i64>,
    parent_rev: &HashMap<i64, i64>,
    edge_meta: &HashMap<(i64, i64), (String, i64)>,
) -> Result<Path> {
    // Front half: src ... meet.
    let mut front: Vec<i64> = vec![meet];
    let mut cur = meet;
    while cur != src {
        let prev = *parent_fwd.get(&cur).expect("parent_fwd should chain back to src");
        front.push(prev);
        cur = prev;
    }
    front.reverse();

    // Back half: meet ... dst (excluding meet, to avoid duplicating it).
    let mut back: Vec<i64> = Vec::new();
    let mut cur = meet;
    while cur != dst {
        let nxt = *parent_rev.get(&cur).expect("parent_rev should chain back to dst");
        back.push(nxt);
        cur = nxt;
    }

    let mut chain: Vec<i64> = front;
    chain.extend(back);

    let mut edges: Vec<(i64, i64, String, i64)> = Vec::with_capacity(chain.len().saturating_sub(1));
    for w in chain.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (kind, line) = edge_meta
            .get(&(a, b))
            .cloned()
            .unwrap_or_else(|| ("unknown".to_string(), 0));
        edges.push((a, b, kind, line));
    }

    let nodes = hydrate_symbols(store, chain)?;
    Ok(Path { nodes, edges })
}

fn hydrate_symbols(store: &Store, ids: Vec<i64>) -> Result<Vec<Symbol>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn();
    // Build the SELECT once with id-keyed lookup; we preserve `ids` ordering
    // by querying each id in turn (the chains are bounded by max_depth so
    // this is at most ~20 queries even on pathological inputs).
    let mut stmt = conn.prepare(
        "SELECT s.name, s.kind, s.signature, f.path, s.line_start, s.line_end, s.visibility \
         FROM symbols s JOIN files f ON s.file_id = f.id \
         WHERE s.id = ?1",
    )?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let sym = stmt
            .query_row(params![id], |row| {
                Ok(Symbol {
                    name: row.get(0)?,
                    kind: kind_from_str(&row.get::<_, String>(1)?),
                    signature: row.get(2)?,
                    parent: None,
                    file: row.get(3)?,
                    line_start: row.get(4)?,
                    line_end: row.get(5)?,
                    visibility: row.get(6)?,
                })
            })
            .ok();
        if let Some(s) = sym {
            out.push(s);
        }
    }
    Ok(out)
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

    /// Fixture: chain a -> b -> c -> d via call edges, plus an unrelated
    /// island (e standalone). Bypasses the extractor — we want to verify
    /// the BFS reconstruction, not the parser.
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
             (4, 1, 'd', 'function', 19, 23), \
             (5, 1, 'e', 'function', 25, 29)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line) VALUES \
             (1, 2, 'call', 3), \
             (2, 3, 'call', 9), \
             (3, 4, 'call', 15)",
            [],
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn why_finds_direct_chain() {
        let (_dir, store) = fixture();
        // a -> d via a -> b -> c -> d. Three edges, depth budget 5.
        let p = why(&store, 1, 4, 5).unwrap().expect("path exists");
        let names: Vec<&str> = p.nodes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"], "got: {names:?}");
        assert_eq!(p.edges.len(), 3, "3 edges in chain: {:?}", p.edges);
        // All edges in this fixture are 'call'.
        assert!(p.edges.iter().all(|(_, _, k, _)| k == "call"));
    }

    #[test]
    fn why_returns_none_for_disconnected_pair() {
        let (_dir, store) = fixture();
        // e is islanded — no path to or from a.
        let p = why(&store, 1, 5, 10).unwrap();
        assert!(p.is_none(), "expected no path, got: {p:?}");
    }

    #[test]
    fn why_respects_max_depth() {
        let (_dir, store) = fixture();
        // a -> d needs 3 hops; budget 2 is insufficient.
        let p = why(&store, 1, 4, 2).unwrap();
        assert!(p.is_none(), "depth 2 should not reach d: {p:?}");
        // Budget 3 succeeds.
        let p = why(&store, 1, 4, 3).unwrap();
        assert!(p.is_some(), "depth 3 should reach d");
    }

    #[test]
    fn why_same_src_dst_returns_singleton() {
        let (_dir, store) = fixture();
        let p = why(&store, 2, 2, 5).unwrap().expect("self-path");
        assert_eq!(p.nodes.len(), 1);
        assert_eq!(p.nodes[0].name, "b");
        assert!(p.edges.is_empty());
    }
}
```

## Constraints

Do not run `cargo build`, `cargo test`, or any other build or test command.

Do not modify any other file. Do not invent extra files.

Then commit with this exact message:

    feat(query): why — bidirectional BFS, path reconstruction
