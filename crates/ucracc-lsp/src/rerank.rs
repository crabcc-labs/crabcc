//! BGE reranker v2 m3, lazy-loaded behind the `rerank` feature.
//!
//! - Cross-encoder: scores (query, doc) pairs, no separate query embedding.
//! - ONNX model lazy-downloaded by fastembed on first use (~1.1 GB).
//! - Cached to `~/.cache/crabcc-memory/` to colocate with MiniLM.
//! - The model handle is held in a `OnceLock` so the second call is free.

#![cfg(feature = "rerank")]

use anyhow::Result;
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
use serde::Serialize;
use std::sync::OnceLock;

static MODEL: OnceLock<std::sync::Mutex<TextRerank>> = OnceLock::new();

fn model() -> Result<&'static std::sync::Mutex<TextRerank>> {
    if let Some(m) = MODEL.get() {
        return Ok(m);
    }
    let cache_dir = dirs_cache_dir().join("crabcc-memory");
    std::fs::create_dir_all(&cache_dir).ok();
    let opts = RerankInitOptions::new(RerankerModel::BGERerankerV2M3).with_cache_dir(cache_dir);
    let m = TextRerank::try_new(opts)?;
    let _ = MODEL.set(std::sync::Mutex::new(m));
    Ok(MODEL.get().expect("just set"))
}

fn dirs_cache_dir() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

#[derive(Debug, Serialize)]
pub struct Scored {
    pub index: usize,
    pub score: f32,
    pub document: String,
}

pub fn rerank_docs(query: &str, docs: &[String], top_n: usize) -> Result<Vec<Scored>> {
    if docs.is_empty() {
        return Ok(Vec::new());
    }
    let m = model()?;
    let mut guard = m
        .lock()
        .map_err(|_| anyhow::anyhow!("rerank mutex poisoned"))?;
    let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let results = guard.rerank(query, doc_refs, true, Some(top_n))?;
    Ok(results
        .into_iter()
        .map(|r| Scored {
            index: r.index,
            score: r.score,
            document: r.document.unwrap_or_default(),
        })
        .collect())
}

#[cfg(feature = "memory")]
pub fn rerank_query_result(
    query: &str,
    mut q: crabcc_memory::QueryResult,
) -> Result<crabcc_memory::QueryResult> {
    // Cap the rerank to the top 50 fusion candidates — the cross-encoder
    // is O(n·model) and beyond 50 the rerank stops paying for itself.
    let pool: usize = 50.min(q.hits.len());
    if pool < 2 {
        return Ok(q);
    }
    let docs: Vec<String> = q.hits[..pool]
        .iter()
        .map(|h| h.drawer.body.clone())
        .collect();
    let scored = rerank_docs(query, &docs, pool)?;
    let mut reordered = Vec::with_capacity(q.hits.len());
    for s in scored {
        if let Some(h) = q.hits.get(s.index) {
            reordered.push(h.clone());
        }
    }
    // Tail stays in fusion order.
    reordered.extend(q.hits.drain(pool..));
    q.hits = reordered;
    Ok(q)
}
