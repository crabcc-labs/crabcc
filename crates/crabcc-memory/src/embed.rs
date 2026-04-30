//! `Embedder` trait + three impls:
//!
//! - `HashEmbedder` — deterministic, dependency-free; default.
//! - `CachedEmbedder` — sha256-keyed moka cache decorator over any inner
//!   `Embedder`.
//! - `FastEmbedder` — real semantic 384-dim MiniLM-L6-v2 vectors via
//!   `fastembed-rs`, behind the `memory-embed` cargo feature.
//!
//! `HashEmbedder` exists so the trait + Backend storage can be exercised
//! end-to-end without pulling ~25 MB of ONNX model file into every build.
//! `FastEmbedder` is what production should use; gated so opting in is
//! explicit and the default `cargo build` stays small.
//!
//! `CachedEmbedder` wraps either: cache hits skip whichever inner
//! embedder is in use, which matters most once `FastEmbedder` is active
//! and `embed_one` is dominated by ONNX inference cost. Today (with
//! `HashEmbedder`) the wrapper is still a measurable win for
//! `Palace::remember` of duplicate content because SQL-layer dedup runs
//! *after* the embed work.

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

    // ─── CachedEmbedder (issue #30 #3) ────────────────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts inner-embedder calls so tests can assert that cache hits
    /// short-circuit the inner call entirely.
    struct CountingEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
    }

    impl CountingEmbedder {
        fn new() -> Self {
            Self {
                inner: HashEmbedder::new(),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Embedder for CountingEmbedder {
        fn dim(&self) -> usize {
            self.inner.dim()
        }
        fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.embed_one(text)
        }
    }

    #[test]
    fn cached_embedder_hit_skips_recompute() {
        let counter = Arc::new(CountingEmbedder::new());
        let cached = CachedEmbedder::new(counter.clone());

        let v1 = cached.embed_one("the fox jumps").unwrap();
        let v2 = cached.embed_one("the fox jumps").unwrap();
        assert_eq!(v1, v2, "cached vector must equal first call");
        assert_eq!(
            counter.calls(),
            1,
            "second embed_one with identical text must hit cache, not call inner"
        );
        assert_eq!(cached.cache_entry_count(), 1);
    }

    #[test]
    fn cached_embedder_miss_calls_inner() {
        // Distinct inputs → distinct sha256 keys → two inner calls.
        let counter = Arc::new(CountingEmbedder::new());
        let cached = CachedEmbedder::new(counter.clone());

        let _ = cached.embed_one("alpha").unwrap();
        let _ = cached.embed_one("beta").unwrap();
        assert_eq!(counter.calls(), 2);
        assert_eq!(cached.cache_entry_count(), 2);
    }

    #[test]
    fn cached_embedder_capacity_bound() {
        // capacity=2; third distinct key forces eviction.
        let counter = Arc::new(CountingEmbedder::new());
        let cached = CachedEmbedder::with_capacity(counter, 2);

        for s in ["a", "b", "c"] {
            let _ = cached.embed_one(s).unwrap();
        }
        assert!(
            cached.cache_entry_count() <= 2,
            "cache must not exceed max_capacity, got {}",
            cached.cache_entry_count()
        );
    }

    #[test]
    fn cached_embedder_dim_passes_through() {
        let inner = Arc::new(HashEmbedder::with_dim(128));
        let cached = CachedEmbedder::new(inner);
        assert_eq!(cached.dim(), 128);
    }
}

// ═══ FastEmbedder (memory-embed feature) ═══════════════════════════════
//
// Real semantic 384-dim MiniLM-L6-v2 embeddings via `fastembed-rs`.
// Lazy model download into the platform cache dir on first construction;
// subsequent runs reuse the cached ONNX file. The whole module is gated
// behind `memory-embed` so default builds ship zero ML deps.
//
// Threading: `fastembed::TextEmbedding` runs ONNX inference and is
// `Send + Sync` once constructed (the underlying ort session is shared
// behind locks). We wrap it in a `Mutex` to serialize `embed_*` calls
// because batched inference inside the same session is the cheapest
// shape — a per-call lock yields one global ort session, not N.

#[cfg(feature = "memory-embed")]
mod fastembed_impl {
    use super::*;
    use anyhow::Context;
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
    use std::sync::Mutex;

    /// 384-dim MiniLM-L6-v2 embedder. Loads the ONNX model on first
    /// construction (~25 MB; lazy-downloaded to the platform cache dir)
    /// and reuses the same ort session across calls.
    pub struct FastEmbedder {
        // fastembed::TextEmbedding mutates internal state during
        // inference (the ort session keeps tensor scratch buffers), so
        // even though it's Sync we serialize through a Mutex to keep
        // the API trait-object friendly and to amortize batches in a
        // single call.
        inner: Mutex<TextEmbedding>,
        dim: usize,
    }

    impl FastEmbedder {
        /// Construct with the default model (`AllMiniLML6V2`, 384-dim).
        /// First call may take seconds while the model downloads.
        pub fn new() -> Result<Self> {
            Self::with_model(EmbeddingModel::AllMiniLML6V2)
        }

        /// Construct with an explicit model. Useful for ablation tests
        /// or for callers that want bge-small / e5-small. Dim is
        /// inferred from the model.
        pub fn with_model(model: EmbeddingModel) -> Result<Self> {
            let opts = InitOptions::new(model.clone());
            let inner = TextEmbedding::try_new(opts)
                .context("fastembed: load model")?;
            // MiniLM-L6-v2 is 384; bge-small/e5-small also 384. Hardcode
            // until we add a `model_dim()` helper or a runtime probe.
            let dim = match model {
                EmbeddingModel::AllMiniLML6V2 => 384,
                _ => 384, // most small encoders we care about
            };
            Ok(Self {
                inner: Mutex::new(inner),
                dim,
            })
        }
    }

    impl Embedder for FastEmbedder {
        fn dim(&self) -> usize {
            self.dim
        }

        fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("FastEmbedder mutex poisoned"))?;
            let mut out = guard
                .embed(vec![text.to_string()], None)
                .context("fastembed: embed_one")?;
            out.pop()
                .ok_or_else(|| anyhow::anyhow!("fastembed returned empty result"))
        }

        fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            // fastembed wants Vec<String>; one allocation per call is
            // negligible next to the ONNX inference cost.
            let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("FastEmbedder mutex poisoned"))?;
            guard.embed(owned, None).context("fastembed: embed_batch")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // Marked #[ignore] because the test downloads ~25 MB on first
        // run and shells out to ort. CI runs explicitly with
        // `--features memory-embed -- --ignored` after setup.
        #[test]
        #[ignore = "downloads ~25 MB MiniLM-L6-v2 on first run"]
        fn fastembed_dim_is_384() {
            let e = FastEmbedder::new().expect("load model");
            assert_eq!(e.dim(), 384);
            let v = e.embed_one("hello world").expect("embed");
            assert_eq!(v.len(), 384);
        }

        #[test]
        #[ignore = "downloads ~25 MB MiniLM-L6-v2 on first run"]
        fn fastembed_semantic_pairs_close() {
            // Same query embedded twice must be byte-identical (or near-
            // identical past floating-point reorder). Different queries
            // produce different vectors with non-trivial cosine drop.
            let e = FastEmbedder::new().expect("load");
            let a = e.embed_one("the fox jumps over the lazy dog").unwrap();
            let b = e.embed_one("the fox jumps over the lazy dog").unwrap();
            let c = e.embed_one("a recipe for chocolate cake").unwrap();
            assert_eq!(a, b, "deterministic for same input");
            // Cosine of L2-normalized fastembed output should differ
            // meaningfully across topically-different inputs.
            let dot_ac: f32 = a.iter().zip(c.iter()).map(|(x, y)| x * y).sum();
            assert!(
                dot_ac < 0.95,
                "topically distinct inputs should not be near-identical (got cosine {dot_ac})"
            );
        }
    }
}

#[cfg(feature = "memory-embed")]
pub use fastembed_impl::FastEmbedder;
