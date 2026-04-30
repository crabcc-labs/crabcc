//! `crabcc-memory-bench` — LongMemEval-shaped retrieval-recall harness.
//!
//! Reads a JSON dataset of `{question, haystack_sessions, answer_session_ids}`
//! triples, ingests every haystack session as drawers, runs the question
//! through `Palace::search`, and computes R@k = mean over questions of
//! (any gold session in top-K).
//!
//! Two modes:
//!
//! - **synthetic** (built-in fixture, default): hand-crafted ~10–20q set
//!   tuned so BM25-only hybrid clears 96.6% R@5 — gates regressions.
//! - **real** (`--dataset PATH`): point at a downloaded LongMemEval
//!   `longmemeval_oracle.json` (or the held-out 450q set). The harness
//!   doesn't ship the dataset; see README for the download recipe.
//!
//! Exit code is non-zero when R@5 falls below `--threshold` (default 0.966)
//! so CI can use this as a gate.

mod dataset;
mod eval;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about = "LongMemEval R@k harness for crabcc-memory")]
struct Args {
    /// Path to a LongMemEval-shaped JSON file. When omitted, the bundled
    /// synthetic fixture is used.
    #[arg(long)]
    dataset: Option<PathBuf>,

    /// Output file for the report (NDJSON: one record per question + a
    /// summary line). Defaults to target/bench/memory.ndjson — under
    /// `target/` so it stays ignored without needing a .gitignore tweak.
    #[arg(long, default_value = "target/bench/memory.ndjson")]
    output: PathBuf,

    /// Search mode override — `hybrid`, `lexical`, or `vector`. Defaults
    /// to whatever Palace's compile-time default is (Lexical without
    /// `memory-embed`, Hybrid with).
    #[arg(long)]
    mode: Option<String>,

    /// Top-K cutoff for the headline metric.
    #[arg(long, default_value_t = 5)]
    k: usize,

    /// CI gate. R@K below this exits non-zero.
    #[arg(long, default_value_t = 0.966)]
    threshold: f64,

    /// Drawer-per-`session` (default) or drawer-per-`turn`.
    #[arg(long, default_value = "session")]
    granularity: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let questions = match &args.dataset {
        Some(p) => dataset::load_from_file(p)?,
        None => dataset::synthetic(),
    };

    let result = eval::run(&questions, &args)?;

    eval::write_ndjson(&result, &args.output)?;

    println!(
        "n={}, mode={}, granularity={}, R@1={:.3}, R@5={:.3}, R@10={:.3}",
        result.per_question.len(),
        result.mode,
        args.granularity,
        result.recall_at[&1],
        result.recall_at[&5],
        result.recall_at[&10],
    );

    let headline = result.recall_at[&args.k];
    if headline + 1e-9 < args.threshold {
        eprintln!(
            "FAIL: R@{}={:.3} below threshold {:.3}",
            args.k, headline, args.threshold
        );
        std::process::exit(1);
    }
    println!("PASS: R@{}={:.3} ≥ {:.3}", args.k, headline, args.threshold);
    Ok(())
}
