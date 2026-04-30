//! Per-question retrieval evaluation.
//!
//! For each question:
//! 1. Open an ephemeral palace.
//! 2. Ingest every haystack session as drawers (one drawer per session
//!    by default, or one per turn-pair when `--granularity turn` is set).
//!    Each drawer's `source_id` carries the session id so retrieval hits
//!    can be mapped back to gold.
//! 3. Run the question through `Palace::search` (or the explicit mode
//!    if `--mode` was passed) at limit = max(top_ks).
//! 4. Mark the question correct@k iff *any* gold session id appears in
//!    the first k hits.

use crate::dataset::{Question, Session};
use anyhow::{Context, Result};
use crabcc_memory::{Palace, SearchMode};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

const TOP_KS: [usize; 3] = [1, 5, 10];

#[derive(Debug, Serialize)]
pub struct PerQuestion {
    pub question_id: String,
    pub gold: Vec<String>,
    pub retrieved: Vec<String>,
    pub hit_at: HashMap<usize, bool>,
}

#[derive(Debug, Serialize)]
pub struct BenchResult {
    pub mode: String,
    pub per_question: Vec<PerQuestion>,
    pub recall_at: HashMap<usize, f64>,
}

pub fn run(questions: &[Question], args: &crate::Args) -> Result<BenchResult> {
    let mode = args
        .mode
        .as_deref()
        .map(|m| {
            SearchMode::parse(m)
                .with_context(|| format!("--mode {m:?} (expected hybrid|lexical|vector)"))
        })
        .transpose()?
        .unwrap_or_default();

    let limit = TOP_KS.iter().max().copied().unwrap_or(10).max(args.k);
    let mut per_question = Vec::with_capacity(questions.len());

    for q in questions {
        let palace = Palace::ephemeral();
        ingest_haystack(&palace, &q.haystack, &args.granularity)?;

        let hits = palace
            .search_with_mode(mode, &q.question, limit, None, None)?
            .hits;
        let retrieved: Vec<String> = hits
            .iter()
            .map(|h| extract_session_id(&h.source_id))
            .collect();

        let mut hit_at: HashMap<usize, bool> = HashMap::new();
        for k in TOP_KS.iter().copied().chain([args.k]) {
            let found = retrieved
                .iter()
                .take(k)
                .any(|sid| q.answer_session_ids.contains(sid));
            hit_at.insert(k, found);
        }

        per_question.push(PerQuestion {
            question_id: q.question_id.clone(),
            gold: q.answer_session_ids.clone(),
            retrieved,
            hit_at,
        });
    }

    let mut recall_at = HashMap::new();
    for k in TOP_KS.iter().copied().chain([args.k]) {
        let n = per_question.len() as f64;
        let hits = per_question
            .iter()
            .filter(|pq| *pq.hit_at.get(&k).unwrap_or(&false))
            .count() as f64;
        recall_at.insert(k, if n > 0.0 { hits / n } else { 0.0 });
    }

    Ok(BenchResult {
        mode: format!("{mode:?}"),
        per_question,
        recall_at,
    })
}

fn ingest_haystack(palace: &Palace, sessions: &[Session], granularity: &str) -> Result<()> {
    for s in sessions {
        match granularity {
            "turn" => {
                let mut pending_user: Option<String> = None;
                let mut pair_idx = 0usize;
                for t in &s.turns {
                    if t.role == "user" {
                        pending_user = Some(t.content.clone());
                    } else if t.role == "assistant" {
                        let user = pending_user.take().unwrap_or_default();
                        pair_idx += 1;
                        let body = format!("USER: {user}\nASSISTANT: {}", t.content);
                        let source_id = format!("{}#{pair_idx}", s.session_id);
                        palace.remember("session", Some("turn"), &source_id, &body)?;
                    }
                }
            }
            _ => {
                // session granularity (default) — one drawer per session,
                // body = newline-joined turns. `source_id` is the bare
                // session id so the harness can recover it directly.
                let body = s
                    .turns
                    .iter()
                    .map(|t| format!("{}: {}", t.role.to_uppercase(), t.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                palace.remember("session", None, &s.session_id, &body)?;
            }
        }
    }
    Ok(())
}

/// Strip the optional `#turn` suffix to recover the source session id.
fn extract_session_id(source_id: &str) -> String {
    match source_id.split_once('#') {
        Some((sid, _)) => sid.to_string(),
        None => source_id.to_string(),
    }
}

pub fn write_ndjson<P: AsRef<Path>>(result: &BenchResult, out: P) -> Result<()> {
    let out = out.as_ref();
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    for pq in &result.per_question {
        buf.push_str(&serde_json::to_string(pq)?);
        buf.push('\n');
    }
    let summary = serde_json::json!({
        "summary": {
            "mode": result.mode,
            "n": result.per_question.len(),
            "recall_at": result.recall_at,
        }
    });
    buf.push_str(&summary.to_string());
    buf.push('\n');
    std::fs::write(out, buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_for_test(mode: Option<&str>) -> crate::Args {
        crate::Args {
            dataset: None,
            output: std::path::PathBuf::from("/tmp/never-written"),
            mode: mode.map(String::from),
            k: 5,
            threshold: 0.0,
            granularity: "session".into(),
        }
    }

    #[test]
    fn synthetic_clears_threshold_in_lexical_mode() {
        // The synthetic fixture is engineered with strong keyword
        // overlap; lexical mode should hit ≥96.6% R@5 reliably without
        // a real embedder. Drops here are a regression in BM25 ranking
        // (likely a tokenizer or fts5 schema change).
        let qs = crate::dataset::synthetic();
        let r = run(&qs, &args_for_test(Some("lexical"))).unwrap();
        let r5 = r.recall_at[&5];
        assert!(
            r5 >= 0.966,
            "synthetic R@5 dropped below 96.6%: got {r5}, per-q: {:?}",
            r.per_question
                .iter()
                .filter(|pq| !*pq.hit_at.get(&5).unwrap_or(&false))
                .map(|pq| (&pq.question_id, &pq.retrieved))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn write_ndjson_emits_summary_line() {
        let qs = crate::dataset::synthetic();
        let r = run(&qs, &args_for_test(Some("lexical"))).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("memory.ndjson");
        write_ndjson(&r, &out).unwrap();
        let written = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        // Per-question lines + one summary line.
        assert_eq!(lines.len(), qs.len() + 1);
        assert!(lines.last().unwrap().contains("\"summary\""));
    }

    #[test]
    fn turn_granularity_recovers_session_id_from_compound_source() {
        // Ingest in `turn` granularity → drawer source_id like "g1#1".
        // The extract_session_id helper strips the suffix; the recall
        // calculation relies on that mapping. Failures here would
        // surface as zero recall on a fixture that's actually solvable.
        assert_eq!(extract_session_id("g1"), "g1");
        assert_eq!(extract_session_id("g1#3"), "g1");
        assert_eq!(extract_session_id("session:abc:5"), "session:abc:5");
    }
}
