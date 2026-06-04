//! Optional Morph LLM integration (https://morphllm.com).
//!
//! **Off unless `MORPH_API_KEY` is set** — privacy gate. Without the key
//! every entry point is a no-op, so no tool output ever leaves the
//! machine. With it:
//!
//!   * [`compact`] — query-conditioned, byte-verbatim compression of a
//!     large tool output (Morph Compact, `POST /v1/compact`). Used by the
//!     PreToolUse rewrite hook to shrink big command output *before* it
//!     reaches the model (PostToolUse can't replace output, so the
//!     compression must happen in the command's own pipeline).
//!   * [`apply`] — Fast Apply (`morph-v3-fast`, `POST /v1/chat/
//!     completions`): merge a lazy edit snippet into a file.
//!
//! Safety: `compact` **never loses output** — on no-key, network error,
//! or a malformed response it returns the original input unchanged. The
//! agent always gets the full output; Morph only ever makes it smaller.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const BASE: &str = "https://api.morphllm.com/v1";

/// Compact-result cache (content-addressed) — turns a repeated identical
/// compaction (agents re-run the same command a lot) from a ~1s network
/// round-trip into a local file read. Separate dir from the index / read
/// / prompt caches; bounded by age + total size.
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const CACHE_CAP_BYTES: u64 = 16 * 1024 * 1024;

fn cache_path(key: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = home.join(".crabcc").join("morph-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(key))
}

/// Cache key = sha256(ratio + query + input). Exact 1:1 — any change in
/// input/query/ratio is a miss.
fn cache_key(input: &str, query: Option<&str>, ratio: f64) -> String {
    crabcc_core::hash::sha256_hex(format!("{ratio}\0{}\0{input}", query.unwrap_or("")).as_bytes())
}

fn cache_get(key: &str) -> Option<String> {
    let p = cache_path(key)?;
    let age = std::fs::metadata(&p)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    (age.as_secs() <= CACHE_TTL_SECS)
        .then(|| std::fs::read_to_string(&p).ok())
        .flatten()
}

fn cache_put(key: &str, val: &str) {
    let Some(p) = cache_path(key) else { return };
    if std::fs::write(&p, val).is_ok() {
        if let Some(dir) = p.parent() {
            prune_cache(dir);
        }
    }
}

/// Evict oldest entries (by mtime) once the cache exceeds the cap.
fn prune_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let total: u64 = entries.iter().map(|(_, len, _)| len).sum();
    if total <= CACHE_CAP_BYTES {
        return;
    }
    entries.sort_by_key(|(mtime, _, _)| *mtime); // oldest first
    let mut over = total - CACHE_CAP_BYTES;
    for (_, len, path) in entries {
        if over == 0 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            over = over.saturating_sub(len);
        }
    }
}

/// The Morph API key, or `None` (integration disabled).
pub fn api_key() -> Option<String> {
    std::env::var("MORPH_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
}

fn post_json(path: &str, key: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let resp = reqwest::Client::new()
            .post(format!("{BASE}{path}"))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .context("morph: request failed")?
            .error_for_status()
            .context("morph: non-2xx response")?;
        let v: serde_json::Value = resp.json().await.context("morph: bad JSON")?;
        Ok(v)
    })
}

/// Query-conditioned compaction. Returns the compacted text, or the
/// original `input` unchanged on any failure (passthrough — never loses
/// the agent's data).
pub fn compact(input: &str, query: Option<&str>, ratio: f64) -> String {
    let Some(key) = api_key() else {
        return input.to_string();
    };
    // Recent identical input -> serve from cache, skip the network round-trip.
    let ck = cache_key(input, query, ratio);
    if let Some(hit) = cache_get(&ck) {
        return hit;
    }
    match try_compact(&key, input, query, ratio) {
        Ok(out) => {
            cache_put(&ck, &out);
            out
        }
        Err(e) => {
            tracing::warn!(target: "crabcc::morph", error = %e, "compact failed; passthrough");
            input.to_string()
        }
    }
}

fn try_compact(key: &str, input: &str, query: Option<&str>, ratio: f64) -> Result<String> {
    let mut body = serde_json::Map::new();
    body.insert("input".into(), input.into());
    body.insert("compression_ratio".into(), ratio.into());
    body.insert("preserve_recent".into(), 0.into());
    if let Some(q) = query.filter(|q| !q.trim().is_empty()) {
        body.insert("query".into(), q.into()); // else Morph auto-detects
    }
    let v = post_json("/compact", key, serde_json::Value::Object(body))?;
    v["output"]
        .as_str()
        .map(|s| s.to_string())
        .context("morph compact: response had no `output` field")
}

/// Fast Apply: merge `update` (a lazy edit snippet) into `code`. Unlike
/// [`compact`] this has no safe passthrough — the merge *is* the
/// operation — so it returns `Err` when disabled or on failure.
pub fn apply(instruction: &str, code: &str, update: &str) -> Result<String> {
    let key = api_key().context("MORPH_API_KEY not set (Morph integration disabled)")?;
    let content = format!(
        "<instruction>{instruction}</instruction>\n<code>{code}</code>\n<update>{update}</update>"
    );
    let body = serde_json::json!({
        "model": "morph-v3-fast",
        "messages": [{ "role": "user", "content": content }],
    });
    let v = post_json("/chat/completions", &key, body)?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .context("morph apply: response had no choices[0].message.content")
}

/// `crabcc morph compact`: read stdin, compact (if enabled + large
/// enough), print to stdout. Always emits *something* — passthrough on
/// no-key / small input / error.
pub fn run_compact(query: Option<&str>, ratio: f64, min_bytes: usize) -> Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    // Don't pay a network round-trip for output that's already small.
    if api_key().is_some() && input.len() >= min_bytes {
        let out = compact(&input, query, ratio);
        // Record the realized reduction so `crabcc track` reflects real
        // session Morph savings (best-effort; only when it actually shrank).
        if out.len() < input.len() {
            crabcc_core::track::record_saved(
                "morph",
                query.unwrap_or(""),
                0,
                "morph",
                crabcc_core::track::tokens_for_bytes(out.len()),
                crabcc_core::track::tokens_for_bytes(input.len() - out.len()),
            );
        }
        print!("{out}");
    } else {
        print!("{input}");
    }
    Ok(())
}

/// `crabcc morph apply`: merge an edit snippet into a file via Fast Apply.
pub fn run_apply(file: &Path, instruction: &str, update: &str, write: bool) -> Result<()> {
    let code = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let merged = apply(instruction, &code, update)?;
    if write {
        std::fs::write(file, &merged).with_context(|| format!("write {}", file.display()))?;
        eprintln!("morph apply: wrote {}", file.display());
    } else {
        print!("{merged}");
    }
    Ok(())
}
