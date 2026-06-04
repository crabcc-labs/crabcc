//! `workspace/executeCommand` handlers — the "knowledge base", "webfetch"
//! and "rerank" surface. Each handler is feature-gated so the binary can
//! be built thin (just the LSP nav surface) or full (with retrieval).

use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

pub const MEMORY_SEARCH: &str = "ucracc.memory.search";
pub const WEBFETCH: &str = "ucracc.webfetch";
pub const RERANK: &str = "ucracc.rerank";
/// Always available: returns a JSON usage/error/perf snapshot.
pub const STATS: &str = "ucracc.stats";

pub fn known_commands() -> Vec<String> {
    let mut v = Vec::new();
    if cfg!(feature = "memory") {
        v.push(MEMORY_SEARCH.to_string());
    }
    if cfg!(feature = "fetch") {
        v.push(WEBFETCH.to_string());
    }
    if cfg!(feature = "rerank") {
        v.push(RERANK.to_string());
    }
    v.push(STATS.to_string());
    v
}

#[cfg(feature = "memory")]
pub fn memory_search(repo_root: &Path, args: &[Value]) -> Result<Value> {
    use crabcc_memory::palace::Palace;
    let query = args
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("memory.search: arg 0 (query) must be a string"))?;
    let limit = args.get(1).and_then(|v| v.as_u64()).unwrap_or(10).min(200) as usize;
    let palace = Palace::open(repo_root)?;
    let result = palace.search(query, limit)?;

    #[cfg(feature = "rerank")]
    let result = crate::rerank::rerank_query_result(query, result)?;

    Ok(serde_json::to_value(&result.hits)?)
}

#[cfg(not(feature = "memory"))]
pub fn memory_search(_repo_root: &Path, _args: &[Value]) -> Result<Value> {
    Ok(json!({"error": "ucracc-lsp built without `memory` feature"}))
}

#[cfg(feature = "fetch")]
pub fn webfetch(args: &[Value]) -> Result<Value> {
    use crabcc_fetch::{fetch_and_clean, FetchOpts};
    let url = args
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("webfetch: arg 0 (url) must be a string"))?
        .to_string();
    let rt = tokio::runtime::Handle::try_current();
    let results = match rt {
        Ok(handle) => {
            let url2 = url.clone();
            handle.block_on(async move { fetch_and_clean(&[url2], FetchOpts::cli()).await })
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async move { fetch_and_clean(&[url], FetchOpts::cli()).await })
        }
    };
    Ok(serde_json::to_value(&results)?)
}

#[cfg(not(feature = "fetch"))]
pub fn webfetch(_args: &[Value]) -> Result<Value> {
    Ok(json!({"error": "ucracc-lsp built without `fetch` feature"}))
}

#[cfg(feature = "rerank")]
pub fn rerank(args: &[Value]) -> Result<Value> {
    let query = args
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("rerank: arg 0 (query) must be a string"))?;
    let docs: Vec<String> = args
        .get(1)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| anyhow::anyhow!("rerank: arg 1 (docs) must be a string array"))?;
    let top_n = args
        .get(2)
        .and_then(|v| v.as_u64())
        .unwrap_or(docs.len() as u64) as usize;
    let scored = crate::rerank::rerank_docs(query, &docs, top_n)?;
    Ok(serde_json::to_value(scored)?)
}

#[cfg(not(feature = "rerank"))]
pub fn rerank(_args: &[Value]) -> Result<Value> {
    Ok(json!({"error": "ucracc-lsp built without `rerank` feature"}))
}
