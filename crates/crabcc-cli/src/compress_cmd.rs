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
}

pub fn run(args: Args) -> Result<()> {
    std::fs::create_dir_all(args.db.parent().unwrap())
        .with_context(|| format!("create index dir for {}", args.db.display()))?;

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
            .prepare("SELECT path FROM files ORDER BY RANDOM()")
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
            .prepare("SELECT name FROM symbols ORDER BY RANDOM()")
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
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {} ({} bytes table on disk)",
        path.display(),
        bytes
    );
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
        if done.is_multiple_of(PROGRESS_EVERY) || done == total {
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
    let plain_bytes = row.0.unwrap_or(0);
    let encoded_bytes = row.1.unwrap_or(0);
    let encoded_rows = row.2;
    let plain_rows = row.3;
    let null_rows = row.4;

    let symbols_path = symbols_path(&args.db);
    let table_bytes = std::fs::metadata(&symbols_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // Best-effort ratio: scale apples-to-apples by per-row average so a
    // partially-rebuilt index still gives a meaningful number.
    let ratio = if encoded_bytes == 0 || encoded_rows == 0 || plain_rows == 0 {
        0.0
    } else {
        let avg_plain = (plain_bytes as f64) / (plain_rows as f64);
        let avg_encoded = (encoded_bytes as f64) / (encoded_rows as f64);
        if avg_encoded > 0.0 {
            avg_plain / avg_encoded
        } else {
            0.0
        }
    };

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
            "ratio": (ratio * 100.0).round() / 100.0,
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
        println!("  estimated ratio (plain/encoded): {:.2}x", ratio);
    }
    let _ = args.root; // currently unused; kept for future per-repo banners
    Ok(())
}
