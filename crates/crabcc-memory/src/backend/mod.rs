//! `Backend` trait + shared cosine helper.
//!
//! Two impls in this module:
//! - `in_memory::InMemoryBackend` — `HashMap` + brute-force, for tests.
//! - `sqlite::SqliteBackend`     — file-backed, brute-force over an
//!   `f32` blob column (default at M0).
//!
//! M0.5 adds `sqlite_vec::SqliteVecBackend` reading the same schema with
//! the `sqlite-vec` extension for ANN. The trait surface is stable across
//! impls — callers only see lower latency on `query`.

use crate::types::*;
use anyhow::Result;
use std::path::Path;

pub mod in_memory;
pub mod sqlite;

pub trait Backend: Send + Sync {
    fn add(&self, drawers: &[DrawerInsert]) -> Result<Vec<DrawerId>>;
    fn query(&self, q: &Query) -> Result<QueryResult>;
    /// Lexical (BM25 / token-overlap) search over drawer bodies. Used by
    /// `Palace::search_hybrid` alongside `query` to drive RRF fusion.
    /// Hits are returned in descending score order; identical row shape
    /// to `query` so callers can blend the two result sets uniformly.
    fn query_lexical(&self, q: &LexicalQuery) -> Result<QueryResult>;
    fn get(&self, ids: &[DrawerId]) -> Result<GetResult>;
    fn delete(&self, sel: &DeleteSel) -> Result<usize>;
    /// Reclaim on-disk space after a delete. Backends that don't compact
    /// (the in-memory one) should return Ok(()) — the trait method is
    /// the contract, not the implementation. SQLite's VACUUM rewrites
    /// the entire DB file, so callers should batch deletes between
    /// `vacuum` calls rather than vacuum-per-row.
    fn vacuum(&self) -> Result<()> {
        Ok(())
    }
    /// Write a transactionally consistent snapshot to `dest` via
    /// `VACUUM INTO`. Default impl returns an error so non-SQLite backends
    /// surface an explicit "not supported" rather than silently no-oping.
    fn vacuum_into(&self, _dest: &Path) -> Result<()> {
        anyhow::bail!("vacuum_into not supported by this backend")
    }
    fn count(&self) -> Result<usize>;
    fn health(&self) -> HealthStatus;
    /// Enumerate drawers without a similarity query. Optional wing filter.
    /// Order is implementation-defined but stable per call (id ASC for SQLite).
    /// `limit == 0` means unlimited.
    fn list_drawers(&self, wing: Option<&str>, limit: usize) -> Result<Vec<Drawer>>;
}

/// Lexical-search input — text query plus the same wing/room/limit knobs as
/// vector `Query`, minus the embedding. Distinct type rather than reusing
/// `Query` so the trait surface makes the two paths obvious to callers
/// and tooling (MCP, future REST).
#[derive(Debug, Clone)]
pub struct LexicalQuery {
    pub text: String,
    pub limit: usize,
    pub wing: Option<String>,
    pub room: Option<String>,
}

/// L2-cosine similarity. Returns 0.0 for length-mismatched or zero vectors.
///
/// Dispatches to a SIMD implementation when the `simd-cosine` feature is
/// on (nightly-only — uses `std::simd::Simd<f32, 8>`). Falls back to the
/// scalar path otherwise. The two paths are agreement-tested in
/// `cosine_simd_matches_scalar` to within `f32::EPSILON * 16`.
///
/// SIMD audit (issue #38): this is the only vector-arithmetic hot path
/// in the workspace. Production embeddings are 384-d (MiniLM-L6-v2) so
/// `Simd<f32, 8>` cuts the iteration count 8× with no scalar tail; the
/// arithmetic itself fully vectorises. The `simd-cosine` cargo feature
/// lights this path on a nightly toolchain while keeping stable users
/// on the scalar fallback — see `docs/RESEARCH-nightly-features.md`.
#[inline]
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    #[cfg(feature = "simd-cosine")]
    {
        cosine_simd(a, b)
    }
    #[cfg(not(feature = "simd-cosine"))]
    {
        cosine_scalar(a, b)
    }
}

/// Scalar cosine — the canonical reference implementation. Always
/// available; the SIMD path uses this for the tail (when length isn't a
/// multiple of the SIMD lane count).
#[inline]
pub(crate) fn cosine_scalar(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(
        a.len(),
        b.len(),
        "cosine_scalar requires equal-length slices"
    );
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// SIMD cosine. Chunks both inputs into `Simd<f32, LANES>` (LANES = 8 —
/// fits AVX2 / NEON / SSE; AVX-512 hosts could lift this to 16 but
/// requires runtime CPU detection we haven't wired up). Tail (when
/// length isn't a multiple of LANES) falls through to scalar.
///
/// Production embeddings are 384-d (MiniLM-L6-v2) which is 48 × 8 — no
/// tail in the hot path. The tail branch only matters for tests using
/// `HashEmbedder::with_dim(N)` with arbitrary N.
///
/// Implementation note: we use `slice::chunks_exact(LANES)` (stable
/// since 1.0) rather than the unstable `Iterator::array_chunks::<LANES>`
/// (rust-lang/rust#100450). Equivalent performance; one less nightly
/// feature gate to carry.
#[cfg(feature = "simd-cosine")]
#[inline]
pub(crate) fn cosine_simd(a: &[f32], b: &[f32]) -> f32 {
    use std::simd::num::SimdFloat;
    use std::simd::Simd;
    debug_assert_eq!(a.len(), b.len(), "cosine_simd requires equal-length slices");
    const LANES: usize = 8;
    let mut dot = Simd::<f32, LANES>::splat(0.0);
    let mut na = Simd::<f32, LANES>::splat(0.0);
    let mut nb = Simd::<f32, LANES>::splat(0.0);

    let mut a_chunks = a.chunks_exact(LANES);
    let mut b_chunks = b.chunks_exact(LANES);
    for (ac, bc) in a_chunks.by_ref().zip(b_chunks.by_ref()) {
        let av = Simd::<f32, LANES>::from_slice(ac);
        let bv = Simd::<f32, LANES>::from_slice(bc);
        dot += av * bv;
        na += av * av;
        nb += bv * bv;
    }
    let mut dot_s = dot.reduce_sum();
    let mut na_s = na.reduce_sum();
    let mut nb_s = nb.reduce_sum();
    // Scalar tail. For 384-d production embeddings this loop executes
    // zero times; only test-only `HashEmbedder::with_dim(N)` for
    // non-multiple-of-8 N pays the cost.
    let a_tail = a_chunks.remainder();
    let b_tail = b_chunks.remainder();
    for (av, bv) in a_tail.iter().zip(b_tail.iter()) {
        dot_s += av * bv;
        na_s += av * av;
        nb_s += bv * bv;
    }
    if na_s == 0.0 || nb_s == 0.0 {
        0.0
    } else {
        dot_s / (na_s.sqrt() * nb_s.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::cosine;

    #[test]
    fn cosine_self_is_one() {
        let v = vec![1.0_f32, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_length_is_zero() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
    }

    #[test]
    fn cosine_zero_is_zero() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    // ---- SIMD agreement gate -----------------------------------------------

    use super::cosine_scalar;
    #[cfg(feature = "simd-cosine")]
    use super::cosine_simd;

    /// Deterministic Lehmer-like PRNG for test data — avoids pulling in
    /// `rand`. Same Park-Miller multiplier as glibc's `rand`.
    fn lehmer(state: &mut u64) -> f32 {
        *state = state.wrapping_mul(48271) % 0x7fffffff;
        (*state as f32 / 0x7fffffff as f32) * 2.0 - 1.0
    }

    fn fill(seed: u64, len: usize) -> Vec<f32> {
        let mut s = seed;
        (0..len).map(|_| lehmer(&mut s)).collect()
    }

    #[cfg(feature = "simd-cosine")]
    #[test]
    fn cosine_simd_matches_scalar_at_dim_384() {
        // Production hot path — MiniLM-L6-v2 dimension. No tail.
        let a = fill(0x12345, 384);
        let b = fill(0xcafe, 384);
        let s = cosine_scalar(&a, &b);
        let v = cosine_simd(&a, &b);
        assert!(
            (s - v).abs() < f32::EPSILON * 16.0,
            "scalar={s}, simd={v}, diff={}",
            (s - v).abs()
        );
    }

    #[cfg(feature = "simd-cosine")]
    #[test]
    fn cosine_simd_matches_scalar_with_tail() {
        // 17 isn't a multiple of 8 — exercises the tail path.
        for &n in &[1, 7, 8, 9, 17, 31, 33, 64, 65, 100, 384, 385] {
            let a = fill(0xa55, n);
            let b = fill(0xb66, n);
            let s = cosine_scalar(&a, &b);
            let v = cosine_simd(&a, &b);
            assert!(
                (s - v).abs() < f32::EPSILON * 32.0,
                "len={n}: scalar={s}, simd={v}, diff={}",
                (s - v).abs()
            );
        }
    }

    #[cfg(feature = "simd-cosine")]
    #[test]
    fn cosine_simd_self_is_one() {
        let v = fill(0xdead_beef, 384);
        let r = cosine_simd(&v, &v);
        assert!(
            (r - 1.0).abs() < f32::EPSILON * 16.0,
            "expected ~1.0, got {r}"
        );
    }

    /// Without the `simd-cosine` feature, `cosine_simd` must not exist.
    /// The scalar path stays the canonical impl. Compiles only on
    /// stable; the assertion is a documentation aid.
    #[cfg(not(feature = "simd-cosine"))]
    #[test]
    fn cosine_falls_back_to_scalar_on_stable() {
        let a = fill(0x1, 384);
        let b = fill(0x2, 384);
        // `cosine()` and `cosine_scalar()` must produce the same number
        // when `simd-cosine` is off.
        assert_eq!(super::cosine(&a, &b), cosine_scalar(&a, &b));
    }

    /// Perf smoke — prints scalar vs SIMD wall-time on a 384-d × 1000-row
    /// workload (representative of the brute-force cosine path used by
    /// `InMemoryBackend::query` / pre-ANN `SqliteBackend::query`). Marked
    /// `#[ignore]` so the regular `cargo test` run skips it; invoke with:
    ///
    ///   cargo test --features simd-cosine \
    ///     -p crabcc-memory backend::tests::cosine_perf_smoke -- --ignored --nocapture
    ///
    /// Output is for human inspection — no assertion (perf swings are
    /// host-dependent). For repeatable numbers use a real harness.
    #[cfg(feature = "simd-cosine")]
    #[test]
    #[ignore = "perf smoke; opt-in via `cargo test --features simd-cosine -- --ignored --nocapture`"]
    fn cosine_perf_smoke() {
        const DIM: usize = 384;
        const N: usize = 1000;
        let query = fill(0xc0ffee, DIM);
        let corpus: Vec<Vec<f32>> = (0..N).map(|i| fill(0x1000 + i as u64, DIM)).collect();

        let t0 = std::time::Instant::now();
        let mut acc_scalar = 0.0_f32;
        for v in &corpus {
            acc_scalar += cosine_scalar(&query, v);
        }
        let scalar_ns = t0.elapsed().as_nanos();

        let t1 = std::time::Instant::now();
        let mut acc_simd = 0.0_f32;
        for v in &corpus {
            acc_simd += cosine_simd(&query, v);
        }
        let simd_ns = t1.elapsed().as_nanos();

        // sanity — the two accumulators must agree to within fp drift.
        assert!(
            (acc_scalar - acc_simd).abs() / acc_scalar.abs().max(1.0) < 1e-3,
            "scalar={acc_scalar}, simd={acc_simd}"
        );

        let speedup = scalar_ns as f64 / simd_ns.max(1) as f64;
        println!(
            "\ncosine perf smoke (DIM={DIM}, N={N}): \
             scalar={scalar_ns}ns  simd={simd_ns}ns  speedup={speedup:.2}x"
        );
    }
}
