//! `Embedder` trait + `HashEmbedder` (deterministic, test-only) +
//! `CachedEmbedder` (sha256-keyed moka cache decorator).
//!
//! M1a ships the hybrid-search storage + fusion layer with
//! `HashEmbedder` driving the vector path. M1b will add `FastEmbedder`
//! (fastembed-rs / MiniLM-L6-v2) behind the `embed-fastembed` feature so
//! the heavyweight ONNX/tokenizer dep tree stays optional. The
//! `Embedder` trait surface is stable across that change — `Backend`
//! impls don't move when the real embedder lands.
//!
//! `CachedEmbedder` wraps any `Embedder` impl and short-circuits
//! repeated calls with identical text. Important once `FastEmbedder`
//! lands and a single `embed_one` is dominated by ONNX inference cost;
//! also a measurable win today on `Palace::remember` of duplicate
//! content (dedup happens at the SQL layer *after* the embed work).

use anyhow::Result;
use moka::sync::Cache;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed_one(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_one(t)).collect()
    }
}

/// Default capacity for the embedding cache. 4 096 entries × 384 f32 ×
/// 4 bytes ≈ 6 MiB upper bound, negligible compared with the ~80 MiB
/// MiniLM session that `FastEmbedder` will keep resident.
pub const DEFAULT_EMBED_CACHE_CAPACITY: u64 = 4_096;

/// Decorator that memoizes `Embedder::embed_one` results keyed by the
/// sha256 of the input text. Wraps any `Embedder` impl; cached vectors
/// live behind `Arc<Vec<f32>>` so cache hits clone the pointer instead
/// of the buffer.
///
/// Cache hits skip the inner embedder entirely, including any ONNX
/// inference once `FastEmbedder` (issue #18) lands. The `embed_one`
/// signature has to return an owned `Vec<f32>` to match the trait, so
/// we clone out of the `Arc` at the boundary — that clone is still
/// cheaper than re-running an embedding pass.
pub struct CachedEmbedder {
    inner: Arc<dyn Embedder>,
    cache: Cache<[u8; 32], Arc<Vec<f32>>>,
}

impl CachedEmbedder {
    /// Wrap `inner` with a default-capacity cache.
    pub fn new(inner: Arc<dyn Embedder>) -> Self {
        Self::with_capacity(inner, DEFAULT_EMBED_CACHE_CAPACITY)
    }

    /// Wrap with an explicit capacity. Useful where the host knows its
    /// embedding workload (a one-shot mining job that wants a large
    /// cache, or a low-memory environment that wants a small one).
    pub fn with_capacity(inner: Arc<dyn Embedder>, capacity: u64) -> Self {
        Self {
            inner,
            cache: Cache::builder().max_capacity(capacity).build(),
        }
    }

    /// Live-entry count after pending evictions are applied. Used by
    /// tests + benches to assert cache state.
    pub fn cache_entry_count(&self) -> u64 {
        self.cache.run_pending_tasks();
        self.cache.entry_count()
    }
}

fn sha256_key(text: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

impl Embedder for CachedEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let key = sha256_key(text);
        // try_get_with coalesces concurrent loads on the same key —
        // only one thread runs the inner embedder, the rest get the
        // cached Arc once it's inserted. Errors propagate via
        // Arc<anyhow::Error> because moka requires its error type to
        // be Clone.
        let arc = self
            .cache
            .try_get_with(key, || self.inner.embed_one(text).map(Arc::new))
            .map_err(|e: Arc<anyhow::Error>| anyhow::anyhow!("embed: {e}"))?;
        Ok((*arc).clone())
    }
}

/// Deterministic non-cryptographic embedder for tests. Hashes the input
/// into a 384-dim L2-normalized vector. NOT semantically meaningful —
/// use only where a stable Embedder impl is needed without pulling
/// `fastembed-rs` (M1).
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new() -> Self {
        Self { dim: 384 }
    }

    pub fn with_dim(dim: usize) -> Self {
        Self { dim }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        // FNV-1a → 64-bit seed → xorshift64 fill. Deterministic per input.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut s = h;
        let mut v = Vec::with_capacity(self.dim);
        for _ in 0..self.dim {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            v.push((s as i32) as f32 / i32::MAX as f32);
        }
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            for x in &mut v {
                *x /= n;
            }
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let e = HashEmbedder::new();
        let a = e.embed_one("hello world").unwrap();
        let b = e.embed_one("hello world").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_differ() {
        let e = HashEmbedder::new();
        let a = e.embed_one("alpha").unwrap();
        let b = e.embed_one("beta").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn l2_normalized() {
        let e = HashEmbedder::new();
        let v = e.embed_one("anything").unwrap();
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-5, "expected unit norm, got {n}");
    }

    #[test]
    fn dim_matches_config() {
        assert_eq!(HashEmbedder::new().dim(), 384);
        assert_eq!(HashEmbedder::with_dim(128).dim(), 128);
        assert_eq!(
            HashEmbedder::with_dim(128).embed_one("x").unwrap().len(),
            128
        );
    }

    #[test]
    fn batch_matches_one_by_one() {
        let e = HashEmbedder::new();
        let texts: Vec<&str> = vec!["alpha", "beta", "gamma"];
        let by_one: Vec<Vec<f32>> = texts.iter().map(|t| e.embed_one(t).unwrap()).collect();
        let by_batch = e.embed_batch(&texts).unwrap();
        assert_eq!(by_one, by_batch);
    }
}
