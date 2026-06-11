//! `crabcc compress` — train an FSST symbol table from the current index,
//! optionally re-encode every existing `signature` row, and report compression
//! statistics. Ships as part of the v2.0.0-alpha FSST integration (issue #1).
//!
//! The command never mutates the index path itself; it writes the trained
//! symbol table next to the DB at `<index_dir>/fsst.symbols` and (when
//! `--rebuild` is passed) updates `symbols.signature` / `signature_enc` in
//! place inside a single transaction-per-1000-rows batch.
//!
//! Read paths are kept independent: training opens the DB with the codec
//! disabled (`Store::open_with_compress(..., false)`) so we always sample
//! plaintext, regardless of whether a stale `.crabcc/fsst.symbols` already
//! exists. Re-encoding likewise opens with the codec disabled and reads the
//! raw bytes via the underlying `rusqlite::Connection`, then encodes with the
//! freshly trained `Codec`.

use anyhow::{Context, Result};
use crabcc_core::compress::Codec;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// Soft cap on total sample bytes fed into `Codec::train`. Research §2 cites
/// "~1 MB ideal"; past that, training time grows without measurable codec
/// quality wins on our corpus shape.
const SAMPLE_BUDGET_BYTES: usize = 1_000_000;
/// Per-column quotas inside the budget. 50 / 30 / 20 mirrors research §7.
const SIG_QUOTA_BYTES: usize = SAMPLE_BUDGET_BYTES / 2;
const PATH_QUOTA_BYTES: usize = (SAMPLE_BUDGET_BYTES * 3) / 10;
const NAME_QUOTA_BYTES: usize = SAMPLE_BUDGET_BYTES / 5;
/// Re-encode batch size — one transaction per `BATCH` rows keeps the WAL
/// reasonable on huge indexes without sacrificing throughput.
const BATCH: usize = 1000;
/// Progress ping cadence during `--rebuild`.
const PROGRESS_EVERY: usize = 5000;

pub struct Args {
    pub root: PathBuf,
    pub db: PathBuf,
    pub rebuild: bool,
    pub stats: bool,
    pub json: bool,
    /// In-process decode-latency probe. When `Some(n)`, sample n encoded rows
    /// and time `Codec::decompress` per row; emit p50/p95/p99 nanoseconds.
    /// Mutually exclusive with `--rebuild` in practice (we just run the probe
    /// and skip training/rebuild).
    pub decode_probe: Option<usize>,
}

pub fn run(args: Args) -> Result<()> {
    std::fs::create_dir_all(args.db.parent().unwrap())
        .with_context(|| format!("create index dir for {}", args.db.display()))?;

    // Probe mode shortcuts everything: load the existing codec from disk,
    // measure raw decompress throughput, exit.
    if let Some(n) = args.decode_probe {
        return decode_probe(&args, n);
    }

    // Stats-only fast path: no training, no rebuild, just query the DB and
    // print. We still open via rusqlite directly so the existing rows decode
    // path stays untouched.
    if args.stats && !args.rebuild {
        return print_stats(&args);
    }

    // Default + --rebuild paths both train (or re-train) the codec first.
    let symbols_path = symbols_path(&args.db);
    let codec = train_codec(&args.db)?;
    let table_bytes = save_codec(&codec, &symbols_path)?;
    let _ = table_bytes; // already announced inside train_codec

    if args.rebuild {
        rebuild_rows(&args.db, &codec)?;
    }

    if args.stats {
        print_stats(&args)?;
    }
    Ok(())
}

fn symbols_path(db: &Path) -> PathBuf {
    db.parent()
        .map(|p| p.join("fsst.symbols"))
        .unwrap_or_else(|| PathBuf::from("fsst.symbols"))
}

/// Sample the index for training input. Stratified per research §7 (50%
/// `symbols.signature`, 30% `files.path`, 20% `symbols.name`); each column is
/// drawn `ORDER BY RANDOM() LIMIT N` and accumulated until its byte quota
/// fills. We deliberately bypass the `Store` codec layer — the input must be
/// plaintext even if a stale symbol table already exists on disk.
fn train_codec(db: &Path) -> Result<Codec> {
    let conn = Connection::open(db).context("open sqlite for sampling")?;
    let mut samples: Vec<Vec<u8>> = Vec::new();

    // signature column — only un-encoded rows yield plaintext we can train on.
    let mut total_sig = 0usize;
    {
        let mut stmt = conn
            .prepare(
                "SELECT signature FROM symbols
                 WHERE signature IS NOT NULL AND signature_enc = 0
                 ORDER BY RANDOM()",
            )
            .context("prepare signature sampler")?;
        let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
        for r in rows {
            let s = match r? {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };
            total_sig += s.len();
            samples.push(s.into_bytes());
            if total_sig >= SIG_QUOTA_BYTES {
                break;
            }
        }
    }

    // file path column — paths repeat path components heavily, great for FSST.
    let mut total_path = 0usize;
    {
        let mut stmt = conn
            .prepare_cached("SELECT path FROM files ORDER BY RANDOM()")
            .context("prepare path sampler")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for r in rows {
            let p = r?;
            if p.is_empty() {
                continue;
            }
            total_path += p.len();
            samples.push(p.into_bytes());
            if total_path >= PATH_QUOTA_BYTES {
                break;
            }
        }
    }

    // symbol name column — short and high-cardinality but adds case variety.
    let mut total_name = 0usize;
    {
        let mut stmt = conn
            .prepare_cached("SELECT name FROM symbols ORDER BY RANDOM()")
            .context("prepare name sampler")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for r in rows {
            let n = r?;
            if n.is_empty() {
                continue;
            }
            total_name += n.len();
            samples.push(n.into_bytes());
            if total_name >= NAME_QUOTA_BYTES {
                break;
            }
        }
    }

    if samples.is_empty() {
        anyhow::bail!(
            "no training samples found in {} — index is empty or only has encoded rows",
            db.display()
        );
    }

    let total_bytes: usize = samples.iter().map(|v| v.len()).sum();
    let sample_count = samples.len();
    let refs: Vec<&[u8]> = samples.iter().map(|v| v.as_slice()).collect();
    let codec = Codec::train(&refs).context("Codec::train")?;
    eprintln!(
        "Trained FSST symbol table: {} samples ({:.1} kB) — sig={}B path={}B name={}B",
        sample_count,
        (total_bytes as f64) / 1024.0,
        total_sig,
        total_path,
        total_name
    );
    Ok(codec)
}

fn save_codec(codec: &Codec, path: &Path) -> Result<u64> {
    codec
        .save(path)
        .with_context(|| format!("save fsst symbol table to {}", path.display()))?;
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or_default();
    println!("wrote {} ({} bytes table on disk)", path.display(), bytes);
    Ok(bytes)
}

/// Re-encode every plaintext `signature` row in `BATCH`-sized transactions.
/// Reads happen with the codec OFF so we never round-trip through a stale
/// table; encode happens in-process with the freshly trained codec.
fn rebuild_rows(db: &Path, codec: &Codec) -> Result<()> {
    let mut conn = Connection::open(db).context("open sqlite for rebuild")?;
    // Snapshot the IDs to rebuild upfront — saves us from chasing an updating
    // cursor while we mutate the same table.
    let ids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM symbols
                 WHERE signature IS NOT NULL AND signature_enc = 0",
            )
            .context("prepare rebuild candidate scan")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let total = ids.len();
    if total == 0 {
        println!("rebuild: nothing to do (all rows already encoded or NULL)");
        return Ok(());
    }
    println!("rebuild: re-encoding {total} rows…");

    let mut bytes_before: u64 = 0;
    let mut bytes_after: u64 = 0;
    let mut done: usize = 0;

    for chunk in ids.chunks(BATCH) {
        let tx = conn.transaction()?;
        {
            // Two prepared stmts share the transaction.
            let mut sel = tx.prepare(
                "SELECT signature FROM symbols
                 WHERE id = ?1 AND signature_enc = 0",
            )?;
            let mut upd = tx.prepare(
                "UPDATE symbols SET signature = ?1, signature_enc = 1
                 WHERE id = ?2",
            )?;
            for &id in chunk {
                let plain: Option<String> = sel
                    .query_row(params![id], |row| row.get::<_, Option<String>>(0))
                    .ok()
                    .flatten();
                let plain = match plain {
                    Some(s) => s,
                    // Row was concurrently NULL'd or already encoded — skip silently.
                    None => continue,
                };
                let encoded = codec.compress(plain.as_bytes());
                bytes_before += plain.len() as u64;
                bytes_after += encoded.len() as u64;
                upd.execute(params![encoded, id])?;
            }
        }
        tx.commit()?;
        done += chunk.len();
        if done % PROGRESS_EVERY == 0 || done == total {
            let pct = if total == 0 {
                100.0
            } else {
                (done as f64) * 100.0 / (total as f64)
            };
            println!("  rebuilt {done} / {total} ({pct:.1}% done)");
        }
    }

    let ratio = if bytes_after == 0 {
        0.0
    } else {
        (bytes_before as f64) / (bytes_after as f64)
    };
    println!(
        "rebuilt {done} rows; bytes_before={bytes_before} bytes_after={bytes_after} ratio={ratio:.2}x"
    );

    // Persist the rebuild totals so `--stats` can report a real ratio after
    // every row has been encoded (the live row counts collapse to plain=0 in
    // that state, which used to produce ratio=0.0 — a measurement artifact,
    // not the true compression). Reading from `meta` survives across runs.
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params!["fsst_last_rebuild_plain_bytes", bytes_before.to_string()],
    )?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params!["fsst_last_rebuild_encoded_bytes", bytes_after.to_string()],
    )?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params!["fsst_last_rebuild_rows", done.to_string()],
    )?;

    // Reclaim the slack pages — UPDATE leaves the file size unchanged because
    // SQLite keeps freed space in the freelist. VACUUM rewrites the file
    // compactly so on-disk size finally reflects the post-rebuild reality.
    println!("rebuild: VACUUM…");
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// In-process decode-latency probe: sample N encoded rows directly via SQL,
/// time `Codec::decompress` per row using `Instant::now()`. Emits p50/p95/p99
/// in NANOSECONDS (compare apples-to-apples with the criterion micro-bench;
/// the previous subprocess loop measured `crabcc sym` end-to-end where SQLite
/// open + tantivy load + JSON encode dominated, hiding the codec entirely).
fn decode_probe(args: &Args, n: usize) -> Result<()> {
    let symbols_path = symbols_path(&args.db);
    if !symbols_path.exists() {
        anyhow::bail!(
            "no symbol table at {} — run `crabcc compress` first",
            symbols_path.display()
        );
    }
    let codec = Codec::load(&symbols_path)
        .with_context(|| format!("load codec from {}", symbols_path.display()))?;
    let conn = Connection::open(&args.db).context("open sqlite for probe")?;
    let mut stmt = conn.prepare(
        "SELECT signature FROM symbols
         WHERE signature IS NOT NULL AND signature_enc = 1
         ORDER BY RANDOM() LIMIT ?1",
    )?;
    let rows: Vec<Vec<u8>> = stmt
        .query_map(params![n as i64], |row| row.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        anyhow::bail!(
            "no encoded rows in {} — run `crabcc compress --rebuild` first",
            args.db.display()
        );
    }

    let mut timings_ns: Vec<u128> = Vec::with_capacity(rows.len());
    let mut total_bytes_in: u128 = 0;
    let mut total_bytes_out: u128 = 0;
    // Warm-up: codec table TLB / branch predictor priming, doesn't go in the sample.
    for r in rows.iter().take(8) {
        let _ = codec.decompress(r);
    }
    for r in &rows {
        let t = std::time::Instant::now();
        let plain = codec.decompress(r);
        let elapsed = t.elapsed().as_nanos();
        timings_ns.push(elapsed);
        total_bytes_in += r.len() as u128;
        total_bytes_out += plain.len() as u128;
    }

    timings_ns.sort_unstable();
    let pct = |p: f64| -> u128 {
        let idx = ((timings_ns.len() as f64 - 1.0) * p).round() as usize;
        timings_ns[idx]
    };
    let p50 = pct(0.50);
    let p95 = pct(0.95);
    let p99 = pct(0.99);
    let min = *timings_ns.first().unwrap();
    let max = *timings_ns.last().unwrap();
    let mean: u128 = timings_ns.iter().sum::<u128>() / (timings_ns.len() as u128);

    if args.json {
        let v = serde_json::json!({
            "samples": timings_ns.len(),
            "p50_ns": p50, "p95_ns": p95, "p99_ns": p99,
            "min_ns": min, "max_ns": max, "mean_ns": mean,
            "total_bytes_in":  total_bytes_in  as u64,
            "total_bytes_out": total_bytes_out as u64,
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("decode-probe: {} samples", timings_ns.len());
        println!("  p50 {p50} ns  p95 {p95} ns  p99 {p99} ns");
        println!("  min {min} ns  max {max} ns  mean {mean} ns");
        println!("  in {total_bytes_in} B → out {total_bytes_out} B");
    }
    Ok(())
}

fn print_stats(args: &Args) -> Result<()> {
    let conn = Connection::open(&args.db).context("open sqlite for stats")?;
    let row: (Option<i64>, Option<i64>, i64, i64, i64) = conn.query_row(
        "SELECT
            SUM(LENGTH(signature)) FILTER (WHERE signature_enc = 0) AS plain_bytes,
            SUM(LENGTH(signature)) FILTER (WHERE signature_enc = 1) AS encoded_bytes,
            COUNT(*) FILTER (WHERE signature_enc = 1)                AS encoded_rows,
            COUNT(*) FILTER (WHERE signature_enc = 0)                AS plain_rows,
            COUNT(*) FILTER (WHERE signature IS NULL)                AS null_rows
         FROM symbols",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let plain_bytes = row.0.unwrap_or_default();
    let encoded_bytes = row.1.unwrap_or_default();
    let encoded_rows = row.2;
    let plain_rows = row.3;
    let null_rows = row.4;

    let symbols_path = symbols_path(&args.db);
    let table_bytes = std::fs::metadata(&symbols_path)
        .map(|m| m.len())
        .unwrap_or_default();

    // Read the persisted post-rebuild totals from `meta` (written by
    // `--rebuild`). When present they give us a true ratio even after every
    // row has been encoded — the live mixed-state ratio collapses in that
    // case because plain_rows = 0.
    let read_meta = |key: &str| -> Option<i64> {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
    };
    let last_plain = read_meta("fsst_last_rebuild_plain_bytes");
    let last_encoded = read_meta("fsst_last_rebuild_encoded_bytes");
    let last_rows = read_meta("fsst_last_rebuild_rows");
    let post_rebuild_ratio = match (last_plain, last_encoded) {
        (Some(p), Some(e)) if e > 0 => Some((p as f64) / (e as f64)),
        _ => None,
    };

    // Best-effort live ratio (mixed state only): scale apples-to-apples by
    // per-row average so a partially-rebuilt index gives a meaningful number.
    let live_ratio = if encoded_bytes == 0 || encoded_rows == 0 || plain_rows == 0 {
        None
    } else {
        let avg_plain = (plain_bytes as f64) / (plain_rows as f64);
        let avg_encoded = (encoded_bytes as f64) / (encoded_rows as f64);
        if avg_encoded > 0.0 {
            Some(avg_plain / avg_encoded)
        } else {
            None
        }
    };
    // Prefer the persisted post-rebuild ratio when both exist — it's exact.
    let ratio = post_rebuild_ratio.or(live_ratio);

    if args.json {
        let v = serde_json::json!({
            "signature": {
                "plain_rows":    plain_rows,
                "plain_bytes":   plain_bytes,
                "encoded_rows":  encoded_rows,
                "encoded_bytes": encoded_bytes,
                "null_rows":     null_rows,
            },
            "symbol_table_bytes": table_bytes,
            "ratio": ratio.map(|r| (r * 100.0).round() / 100.0),
            "post_rebuild": last_plain.map(|_| serde_json::json!({
                "plain_bytes":   last_plain,
                "encoded_bytes": last_encoded,
                "rows":          last_rows,
                "ratio":         post_rebuild_ratio.map(|r| (r * 100.0).round() / 100.0),
            })),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("crabcc compress --stats");
        println!("  signature column:");
        println!(
            "    plain   : {:>7} rows / {:>10} bytes",
            plain_rows, plain_bytes
        );
        println!(
            "    encoded : {:>7} rows / {:>10} bytes",
            encoded_rows, encoded_bytes
        );
        println!("    null    : {:>7} rows", null_rows);
        println!("  symbol table on disk: {} bytes", table_bytes);
        match ratio {
            Some(r) => println!("  estimated ratio (plain/encoded): {r:.2}x"),
            None => println!("  estimated ratio (plain/encoded): n/a (need both plain and encoded rows or a prior --rebuild)"),
        }
        if let (Some(p), Some(e), Some(rows)) = (last_plain, last_encoded, last_rows) {
            let r = post_rebuild_ratio.unwrap_or(0.0);
            println!("  last --rebuild: {rows} rows, {p}B → {e}B (ratio {r:.2}x)");
        }
    }
    let _ = args.root; // currently unused; kept for future per-repo banners
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_path_in_subdirectory() {
        let db = PathBuf::from("/home/user/.crabcc/index.db");
        let result = symbols_path(&db);
        assert_eq!(result, PathBuf::from("/home/user/.crabcc/fsst.symbols"));
    }

    #[test]
    fn symbols_path_bare_filename() {
        let db = PathBuf::from("index.db");
        // When db has no parent, fallback
        let result = symbols_path(&db);
        assert_eq!(result, PathBuf::from("fsst.symbols"));
    }

    #[test]
    fn symbols_path_nested() {
        let db = PathBuf::from("/a/b/c/d.db");
        assert_eq!(symbols_path(&db), PathBuf::from("/a/b/c/fsst.symbols"));
    }

    #[test]
    fn args_struct_defaults() {
        let args = Args {
            root: PathBuf::from("/tmp"),
            db: PathBuf::from("/tmp/.crabcc/index.db"),
            rebuild: false,
            stats: false,
            json: false,
            decode_probe: None,
        };
        assert!(!args.rebuild);
        assert!(!args.stats);
        assert!(args.decode_probe.is_none());
    }

    #[test]
    fn run_on_empty_db_errors() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        // Create an empty SQLite DB with the expected schema
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                name TEXT,
                signature TEXT,
                signature_enc INTEGER DEFAULT 0,
                file_id INTEGER
            );
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT
            );
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT
            );",
        )
        .unwrap();
        drop(conn);

        let args = Args {
            root: dir.path().to_path_buf(),
            db: db_path,
            rebuild: false,
            stats: false,
            json: false,
            decode_probe: None,
        };
        // With no rows, train_codec should fail
        let result = run(args);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no training samples"));
    }

    #[test]
    fn stats_on_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                name TEXT,
                signature TEXT,
                signature_enc INTEGER DEFAULT 0,
                file_id INTEGER
            );
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT
            );
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT
            );",
        )
        .unwrap();
        drop(conn);

        let args = Args {
            root: dir.path().to_path_buf(),
            db: db_path,
            rebuild: false,
            stats: true,
            json: true,
            decode_probe: None,
        };
        // stats-only on empty DB should work (just reports zeros)
        let result = run(args);
        assert!(result.is_ok());
    }

    #[test]
    fn decode_probe_no_symbol_table_errors() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                name TEXT,
                signature TEXT,
                signature_enc INTEGER DEFAULT 0
            );",
        )
        .unwrap();
        drop(conn);

        let args = Args {
            root: dir.path().to_path_buf(),
            db: db_path,
            rebuild: false,
            stats: false,
            json: false,
            decode_probe: Some(10),
        };
        let result = run(args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no symbol table"));
    }
}
