//! KG op: blast-radius. Reverse-direction transitive walk over `edges`
//! starting at `root_symbol_id`. Returns every symbol that transitively
//! points at the root (directly or via intermediates) up to `max_depth`,
//! plus the smallest depth at which each was reached.
//!
//! v4 edge schema:
//!   edges(id, src_symbol_id, dst_symbol_id, kind, line)
//!   indexed by (dst_symbol_id, kind) — the BFS frontier picks up the
//!   index automatically.
//!
//! `kinds` filters edges by kind (`call`, `ref`, `import`, `inherit`,
//! `impl`). An empty `kinds` slice means "all kinds" — the caller is
//! responsible for the meaning, e.g. `&["call"]` for call-chain blast,
//! `&[]` for everything.

use crate::store::Store;
use crate::types::{Symbol, SymbolKind};
use anyhow::Result;
use rusqlite::params_from_iter;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Serialize)]
pub struct BlastRadiusResult {
    /// Symbols transitively reaching `root_symbol_id` via incoming edges.
    /// The root itself is NOT included; only affected dependents.
    pub affected: Vec<Symbol>,
    /// `symbol_id -> smallest depth at which the BFS reached it`.
    /// Depth 1 = direct caller/referer of the root.
    pub depth_map: HashMap<i64, usize>,
    /// Echo of the `kinds` filter used (or the resolved full kind list
    /// when the caller passed an empty slice). Useful for callers that
    /// want to surface "what we walked" in CLI/MCP output.
    pub kinds_used: Vec<String>,
}

/// BFS over reverse edges from `root_symbol_id`, capped at `max_depth`.
/// `kinds` empty means "no kind filter".
pub fn blast_radius(
    store: &Store,
    root_symbol_id: i64,
    max_depth: usize,
    kinds: &[&str],
) -> Result<BlastRadiusResult> {
    let kinds_used: Vec<String> = kinds.iter().map(|s| s.to_string()).collect();
    let mut depth_map: HashMap<i64, usize> = HashMap::new();
    let mut seen: HashSet<i64> = HashSet::new();
    seen.insert(root_symbol_id);

    if max_depth == 0 {
        return Ok(BlastRadiusResult {
            affected: Vec::new(),
            depth_map,
            kinds_used,
        });
    }

    let conn = store.conn();
    // We build the SQL once and bind the dst id per frontier hop. With kinds
    // filtering we emit `kind IN (?, ?, ...)`; without, we drop the clause.
    let sql = if kinds.is_empty() {
        "SELECT DISTINCT src_symbol_id FROM edges WHERE dst_symbol_id = ?1".to_string()
    } else {
        let placeholders: Vec<String> = (0..kinds.len()).map(|i| format!("?{}", i + 2)).collect();
        format!(
            "SELECT DISTINCT src_symbol_id FROM edges \
             WHERE dst_symbol_id = ?1 AND kind IN ({})",
            placeholders.join(",")
        )
    };
    let mut stmt = conn.prepare(&sql)?;

    let mut frontier: VecDeque<(i64, usize)> = VecDeque::new();
    frontier.push_back((root_symbol_id, 0));

    while let Some((node, depth)) = frontier.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let next_depth = depth + 1;

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(node)];
        for k in kinds {
            params.push(Box::new(k.to_string()));
        }
        let rows = stmt.query_map(params_from_iter(params.iter().map(|b| b.as_ref())), |row| {
            row.get::<_, i64>(0)
        })?;
        for r in rows {
            let src = r?;
            if seen.insert(src) {
                depth_map.insert(src, next_depth);
                frontier.push_back((src, next_depth));
            }
        }
    }

    // Hydrate affected symbol records in a single SQL call. The ids vec is
    // bounded by `max_depth` × fan-in, in practice well under thousands.
    let affected = hydrate_symbols(store, depth_map.keys().copied().collect())?;

    Ok(BlastRadiusResult {
        affected,
        depth_map,
        kinds_used,
    })
}

fn hydrate_symbols(store: &Store, ids: Vec<i64>) -> Result<Vec<Symbol>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn();
    let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT s.name, s.kind, s.signature, NULL, f.path, s.line_start, s.line_end, s.visibility \
         FROM symbols s JOIN files f ON s.file_id = f.id \
         WHERE s.id IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::ToSql>> = ids
        .into_iter()
        .map(|i| Box::new(i) as Box<dyn rusqlite::ToSql>)
        .collect();
    let rows = stmt.query_map(params_from_iter(params.iter().map(|b| b.as_ref())), |row| {
        Ok(Symbol {
            name: row.get(0)?,
            kind: kind_from_str(&row.get::<_, String>(1)?),
            signature: row.get(2)?,
            parent: row.get(3)?,
            file: row.get(4)?,
            line_start: row.get(5)?,
            line_end: row.get(6)?,
            visibility: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

    /// Build a minimal v4 fixture by hand: 3 files, 4 symbols, edges
    /// shaped as `c -> b -> a` (a is the root; b is depth 1 from a; c
    /// is depth 2 from a). Bypasses the extractor — we want to verify
    /// the BFS, not the parser.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        let conn = store.conn();
        // files
        conn.execute(
            "INSERT INTO files(path, sha256, mtime, lang, indexed_at) \
             VALUES ('a.rs','x',0,'rust',0),('b.rs','y',0,'rust',0),('c.rs','z',0,'rust',0)",
            [],
        )
        .unwrap();
        // symbols
        conn.execute(
            "INSERT INTO symbols(id, file_id, name, kind, line_start, line_end) VALUES \
             (1, 1, 'a', 'function', 1, 5), \
             (2, 2, 'b', 'function', 1, 5), \
             (3, 3, 'c', 'function', 1, 5), \
             (4, 3, 'd', 'function', 6, 10)",
            [],
        )
        .unwrap();
        // edges: c -> b (call), b -> a (call), d -> a (ref)
        conn.execute(
            "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line) VALUES \
             (3, 2, 'call', 3), \
             (2, 1, 'call', 3), \
             (4, 1, 'ref', 7)",
            [],
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn blast_radius_walks_reverse_chain_to_depth() {
        let (_dir, store) = fixture();
        // depth=1 from a → b and d (both have direct edges to a).
        let r = blast_radius(&store, 1, 1, &[]).unwrap();
        let ids: HashSet<i64> = r.depth_map.keys().copied().collect();
        assert!(ids.contains(&2), "b should be depth 1: {:?}", r.depth_map);
        assert!(ids.contains(&4), "d should be depth 1: {:?}", r.depth_map);
        assert_eq!(r.depth_map[&2], 1);
        assert_eq!(r.depth_map[&4], 1);

        // depth=2 also catches c via b.
        let r = blast_radius(&store, 1, 2, &[]).unwrap();
        assert!(
            r.depth_map.contains_key(&3),
            "c should be depth 2: {:?}",
            r.depth_map
        );
        assert_eq!(r.depth_map[&3], 2);
        assert_eq!(r.affected.len(), 3, "b, c, d expected: {:?}", r.affected);
    }

    #[test]
    fn blast_radius_filters_by_kind() {
        let (_dir, store) = fixture();
        // Only 'call' edges: b → a is a call, d → a is a ref. Filtering to
        // call drops d entirely, and c still shows up via b (call chain).
        let r = blast_radius(&store, 1, 5, &["call"]).unwrap();
        assert!(r.depth_map.contains_key(&2), "b reachable via call");
        assert!(r.depth_map.contains_key(&3), "c reachable via b's call");
        assert!(
            !r.depth_map.contains_key(&4),
            "d only has ref edge — should be filtered"
        );
        assert_eq!(r.kinds_used, vec!["call".to_string()]);
    }

    #[test]
    fn blast_radius_zero_depth_returns_empty() {
        let (_dir, store) = fixture();
        let r = blast_radius(&store, 1, 0, &[]).unwrap();
        assert!(r.affected.is_empty());
        assert!(r.depth_map.is_empty());
    }

    #[test]
    fn blast_radius_unknown_root_returns_empty() {
        let (_dir, store) = fixture();
        let r = blast_radius(&store, 999, 5, &[]).unwrap();
        assert!(r.affected.is_empty());
        assert!(r.depth_map.is_empty());
    }
}
