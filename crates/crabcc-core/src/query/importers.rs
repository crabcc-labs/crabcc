//! KG op: importers. File-level reverse rollup over `edges` — for a
//! target file, returns every file that transitively contains a symbol
//! pointing at a symbol in the target. BFS at file granularity, bounded
//! by `max_depth`. The smallest depth at which each source file is
//! reached is reported in `depth`; the total number of edges pointing at
//! the target (or transitively at target-reached files) is in
//! `edge_count`.
//!
//! v4 edge schema:
//!   edges(id, src_symbol_id, dst_symbol_id, kind, line)
//!   symbols(id, file_id, ...) — joined per BFS hop to map symbol → file.

use crate::store::Store;
use anyhow::Result;
use rusqlite::{params, params_from_iter};
use serde::Serialize;
use ahash::{HashMap, HashSet};

#[derive(Debug, Serialize)]
pub struct FileImporter {
    pub file_path: String,
    /// Number of edges from this file's symbols to symbols in any file
    /// that was already in the "reached" set at the time we expanded
    /// to this file. Sums over all edges crossing into the target frontier.
    pub edge_count: usize,
    /// BFS depth at which this file was first reached. 1 = direct importer
    /// of the target file; 2 = importer of an importer; etc.
    pub depth: usize,
}

pub fn importers(store: &Store, target_path: &str, max_depth: usize) -> Result<Vec<FileImporter>> {
    let conn = store.conn();

    // Resolve target file_id. Unknown path → empty result.
    let target_file_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![target_path],
            |row| row.get::<_, i64>(0),
        )
        .ok();
    let Some(target_file_id) = target_file_id else {
        return Ok(Vec::new());
    };

    if max_depth == 0 {
        return Ok(Vec::new());
    }

    // `reached` is the file-id set the BFS has already expanded into,
    // starting with just the target. We accumulate per-file (depth,
    // edge_count) only for files OUTSIDE the initial target.
    let mut reached: HashSet<i64> = HashSet::default();
    reached.insert(target_file_id);

    // result[file_id] = (depth, edge_count). We aggregate edge_count across
    // BFS waves: a file may be reached at depth d but accumulate additional
    // edges in later waves as the reached set grows.
    let mut result: HashMap<i64, (usize, usize)> = HashMap::default();

    // Frontier per wave: the file-ids whose imports we are about to expand.
    let mut frontier: HashSet<i64> = HashSet::default();
    frontier.insert(target_file_id);

    // SQL: count edges whose destination symbol lives in ANY of the
    // current `reached` file-ids, grouped by source file. Uses IN (?, ?, …)
    // with a placeholder per reached id.
    for depth in 1..=max_depth {
        if frontier.is_empty() {
            break;
        }
        // Build the SQL fresh each wave because the IN-list size changes.
        let dst_ids: Vec<i64> = reached.iter().copied().collect();
        let placeholders: Vec<String> = (0..dst_ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT s_src.file_id, COUNT(*) AS edges \
             FROM edges e \
             JOIN symbols s_src ON s_src.id = e.src_symbol_id \
             JOIN symbols s_dst ON s_dst.id = e.dst_symbol_id \
             WHERE s_dst.file_id IN ({list}) \
               AND s_src.file_id NOT IN ({list}) \
             GROUP BY s_src.file_id",
            list = placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound: Vec<Box<dyn rusqlite::ToSql>> = dst_ids
            .iter()
            .map(|i| Box::new(*i) as Box<dyn rusqlite::ToSql>)
            .collect();
        let rows = stmt.query_map(params_from_iter(bound.iter().map(|b| b.as_ref())), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        let mut next_frontier: HashSet<i64> = HashSet::default();
        for r in rows {
            let (src_file_id, edges) = r?;
            // Only files NOT already reached become new frontier entries —
            // but their edge contribution is added regardless (a file can
            // pile up edges across waves as new dst files join `reached`).
            let entry = result.entry(src_file_id).or_insert((depth, 0));
            // depth is set on first insert, not on subsequent updates.
            entry.1 += edges;
            if !reached.contains(&src_file_id) {
                next_frontier.insert(src_file_id);
            }
        }
        // Promote the new frontier into reached BEFORE the next wave so
        // dst membership reflects everything found so far.
        for f in &next_frontier {
            reached.insert(*f);
        }
        frontier = next_frontier;
    }

    // Hydrate file_id → path. Single SQL.
    let mut out: Vec<FileImporter> = Vec::with_capacity(result.len());
    {
        let ids: Vec<i64> = result.keys().copied().collect();
        if !ids.is_empty() {
            let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT id, path FROM files WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = conn.prepare(&sql)?;
            let bound: Vec<Box<dyn rusqlite::ToSql>> = ids
                .iter()
                .map(|i| Box::new(*i) as Box<dyn rusqlite::ToSql>)
                .collect();
            let rows = stmt
                .query_map(params_from_iter(bound.iter().map(|b| b.as_ref())), |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
            for r in rows {
                let (id, path) = r?;
                if let Some((depth, edge_count)) = result.get(&id).copied() {
                    out.push(FileImporter {
                        file_path: path,
                        edge_count,
                        depth,
                    });
                }
            }
        }
    }
    // Stable ordering: depth ASC, edge_count DESC, path ASC. Makes the
    // output reproducible for the integration test fingerprint.
    out.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(b.edge_count.cmp(&a.edge_count))
            .then(a.file_path.cmp(&b.file_path))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// Fixture: 4 files (target.rs, importer_a.rs, importer_b.rs,
    /// transitive.rs). Edges:
    ///   importer_a.rs → target.rs (2 edges)
    ///   importer_b.rs → target.rs (1 edge)
    ///   transitive.rs → importer_a.rs (1 edge)
    /// Bypasses the extractor — we want to verify the BFS rollup, not
    /// the parser.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("idx.db")).unwrap();
        let conn = store.conn();
        conn.execute(
            "INSERT INTO files(path, sha256, mtime, lang, indexed_at) VALUES \
             ('target.rs','t',0,'rust',0), \
             ('importer_a.rs','a',0,'rust',0), \
             ('importer_b.rs','b',0,'rust',0), \
             ('transitive.rs','c',0,'rust',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(id, file_id, name, kind, line_start, line_end) VALUES \
             (1, 1, 'target_fn',    'function', 1, 5), \
             (2, 1, 'target_type',  'struct',   7, 11), \
             (3, 2, 'importer_a_fn','function', 1, 5), \
             (4, 3, 'importer_b_fn','function', 1, 5), \
             (5, 4, 'transitive_fn','function', 1, 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges(src_symbol_id, dst_symbol_id, kind, line) VALUES \
             (3, 1, 'call', 3), \
             (3, 2, 'ref',  4), \
             (4, 1, 'call', 3), \
             (5, 3, 'call', 3)",
            [],
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn importers_finds_direct_dependents_at_depth_1() {
        let (_dir, store) = fixture();
        let r = importers(&store, "target.rs", 1).unwrap();
        // depth 1: importer_a.rs (2 edges), importer_b.rs (1 edge).
        // transitive.rs is depth 2 — not reached here.
        let paths: Vec<&str> = r.iter().map(|f| f.file_path.as_str()).collect();
        assert!(paths.contains(&"importer_a.rs"), "got: {paths:?}");
        assert!(paths.contains(&"importer_b.rs"), "got: {paths:?}");
        assert!(!paths.contains(&"transitive.rs"), "got: {paths:?}");

        let a = r.iter().find(|f| f.file_path == "importer_a.rs").unwrap();
        assert_eq!(a.edge_count, 2);
        assert_eq!(a.depth, 1);
        let b = r.iter().find(|f| f.file_path == "importer_b.rs").unwrap();
        assert_eq!(b.edge_count, 1);
        assert_eq!(b.depth, 1);
    }

    #[test]
    fn importers_walks_transitive_at_depth_2() {
        let (_dir, store) = fixture();
        let r = importers(&store, "target.rs", 2).unwrap();
        let paths: Vec<&str> = r.iter().map(|f| f.file_path.as_str()).collect();
        assert!(paths.contains(&"transitive.rs"), "got: {paths:?}");

        let t = r.iter().find(|f| f.file_path == "transitive.rs").unwrap();
        assert_eq!(t.depth, 2);
        assert_eq!(t.edge_count, 1, "one edge: transitive → importer_a");
    }

    #[test]
    fn importers_unknown_path_returns_empty() {
        let (_dir, store) = fixture();
        let r = importers(&store, "does_not_exist.rs", 5).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn importers_zero_depth_returns_empty() {
        let (_dir, store) = fixture();
        let r = importers(&store, "target.rs", 0).unwrap();
        assert!(r.is_empty());
    }
}
