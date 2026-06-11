//! int8 and 1-bit (binary) quantization for embedding blobs.
//!
//! Two codecs live here: symmetric per-vector int8 (~3.96x, the no-vec
//! default) and 1-bit sign quantization (32x, opt-in for edge devices,
//! scored by Hamming distance — see the binary section below).
//!
//! Embeddings are L2-normalized f32 vectors (MiniLM-L6-v2 / `HashEmbedder`),
//! so every component sits in `[-1, 1]`. Storing them as raw f32 costs
//! `4 × dim` bytes (1 536 B at dim 384). int8 quantization records a single
//! per-vector scale plus one signed byte per component:
//!
//! ```text
//! [ scale: f32 LE ][ q0: i8 ][ q1: i8 ] … [ q{dim-1}: i8 ]
//! ```
//!
//! That is `4 + dim` bytes (388 B at dim 384) — a ~3.96x reduction. We use
//! a per-vector max-abs scale rather than a global constant so the full int8
//! range is used regardless of how peaky a given vector is; the round-trip
//! error stays below `scale/2` per component, which preserves cosine
//! similarity to >0.999 on real 384-d embeddings (see tests).
//!
//! This is the codec only — wiring into the SQLite backend's `bytes` column
//! lives in `backend::sqlite` and is gated so the f32 sqlite-vec mirror is
//! never fed quantized bytes.

/// Quantize an f32 vector into a `[scale: f32 LE][i8; dim]` blob.
///
/// A zero vector (or any vector whose components are all 0) yields a blob
/// with `scale = 0.0` and all-zero bytes; `dequantize_i8` reconstructs zeros
/// from it, so the round-trip is lossless in that degenerate case.
pub fn quantize_i8(v: &[f32]) -> Vec<u8> {
    let max_abs = v.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
    // scale maps the largest-magnitude component onto ±127. When the input
    // is all zeros, scale stays 0 and every quantized byte is 0.
    let scale = if max_abs > 0.0 { max_abs / 127.0 } else { 0.0 };

    let mut out = Vec::with_capacity(4 + v.len());
    out.extend_from_slice(&scale.to_le_bytes());
    for &x in v {
        let q = if scale > 0.0 {
            // round-to-nearest, then clamp into i8 range.
            (x / scale).round().clamp(-127.0, 127.0) as i8
        } else {
            0
        };
        out.push(q as u8);
    }
    out
}

/// Reconstruct an f32 vector from a `quantize_i8` blob. Returns an empty
/// vector for a malformed (sub-4-byte) blob.
pub fn dequantize_i8(blob: &[u8]) -> Vec<f32> {
    if blob.len() < 4 {
        return Vec::new();
    }
    let scale = f32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    blob[4..]
        .iter()
        .map(|&b| (b as i8) as f32 * scale)
        .collect()
}

// ─── 1-bit (binary) quantization ────────────────────────────────────────
//
// The extreme edge case: keep only the sign of each component, one bit per
// dim, packed `ceil(dim/8)` bytes — 48 B at dim 384, a 32x reduction vs f32.
// Magnitude is discarded, so this is a coarse approximation: similarity is
// measured by Hamming distance (popcount of the XOR), which for sign-bit
// vectors tracks cosine as `cos ≈ 1 − 2·hamming/dim`. Recall is lower than
// int8 — intended for memory-bound edge devices, opt-in via the
// `CRABCC_EMBED_QUANT=binary` env var, never the default.

/// Pack the sign bits of `v` into `ceil(dim/8)` bytes. Bit `i` (LSB-first
/// within each byte) is 1 when `v[i] >= 0.0`. Unused high bits of the final
/// byte stay 0, so two blobs of the same dim agree on padding and it never
/// affects a Hamming comparison.
pub fn quantize_binary(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len().div_ceil(8)];
    for (i, &x) in v.iter().enumerate() {
        if x >= 0.0 {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// Reconstruct an L2-normalized ±1 vector from a `quantize_binary` blob.
/// Lossy (magnitude is gone); used only to feed a quant=2 row into the
/// f32-only sqlite-vec mirror when a quantized db is opened by a vec build.
pub fn dequantize_binary(blob: &[u8], dim: usize) -> Vec<f32> {
    let norm = if dim > 0 {
        1.0 / (dim as f32).sqrt()
    } else {
        0.0
    };
    (0..dim)
        .map(|i| {
            let bit = (blob[i / 8] >> (i % 8)) & 1;
            if bit == 1 {
                norm
            } else {
                -norm
            }
        })
        .collect()
}

/// Cosine-equivalent similarity of two `quantize_binary` blobs over `dim`
/// bits: `1 − 2·hamming/dim`, in `[-1, 1]` (higher = better, matching the
/// cosine convention used by the brute-force ranker). Padding bits are 0 in
/// both blobs so they contribute no Hamming distance.
pub fn hamming_score(a: &[u8], b: &[u8], dim: usize) -> f32 {
    if dim == 0 || a.len() != b.len() {
        return 0.0;
    }
    let hamming: u32 = a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum();
    1.0 - 2.0 * (hamming as f32) / (dim as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Park-Miller PRNG → unit-norm vector. Mirrors the helper used by the
    /// cosine tests so we avoid a `rand` dependency.
    fn unit_vec(seed: u64, dim: usize) -> Vec<f32> {
        let mut s = seed;
        let mut v: Vec<f32> = (0..dim)
            .map(|_| {
                s = s.wrapping_mul(48271) % 0x7fffffff;
                (s as f32 / 0x7fffffff as f32) * 2.0 - 1.0
            })
            .collect();
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            for x in &mut v {
                *x /= n;
            }
        }
        v
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    }

    #[test]
    fn blob_is_four_plus_dim_bytes() {
        let v = unit_vec(0x1234, 384);
        let blob = quantize_i8(&v);
        assert_eq!(blob.len(), 4 + 384);
        // ~3.96x smaller than the f32 encoding (4 * 384 = 1536 B).
        assert!(blob.len() * 3 < 384 * 4);
    }

    #[test]
    fn roundtrip_error_below_half_scale() {
        let v = unit_vec(0xcafe, 384);
        let back = dequantize_i8(&quantize_i8(&v));
        assert_eq!(back.len(), v.len());
        let max_abs = v.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        let scale = max_abs / 127.0;
        // round-to-nearest guarantees per-component error <= scale/2 (plus a
        // small f32 slack).
        for (o, b) in v.iter().zip(&back) {
            assert!(
                (o - b).abs() <= scale / 2.0 + 1e-6,
                "component error {} exceeded scale/2 {}",
                (o - b).abs(),
                scale / 2.0
            );
        }
    }

    #[test]
    fn cosine_preserved_above_point_999() {
        // The property that matters for KNN ranking: quantizing must not
        // meaningfully move a vector's direction.
        for seed in [0x1u64, 0x2, 0xbeef, 0xdead, 0x9999] {
            let v = unit_vec(seed, 384);
            let back = dequantize_i8(&quantize_i8(&v));
            let c = cosine(&v, &back);
            assert!(c > 0.999, "cosine after quant was {c} for seed {seed:#x}");
        }
    }

    #[test]
    fn zero_vector_roundtrips_to_zeros() {
        let v = vec![0.0_f32; 384];
        let blob = quantize_i8(&v);
        let back = dequantize_i8(&blob);
        assert_eq!(back, v);
    }

    #[test]
    fn malformed_blob_returns_empty() {
        assert!(dequantize_i8(&[]).is_empty());
        assert!(dequantize_i8(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn handles_arbitrary_dim() {
        for dim in [1, 7, 8, 128, 385] {
            let v = unit_vec(0x55, dim);
            let back = dequantize_i8(&quantize_i8(&v));
            assert_eq!(back.len(), dim);
        }
    }

    // ─── binary (1-bit) quantization ──────────────────────────────────

    #[test]
    fn binary_blob_is_ceil_dim_over_8_bytes() {
        assert_eq!(quantize_binary(&unit_vec(0x1, 384)).len(), 48); // 32x vs 1536
        assert_eq!(quantize_binary(&unit_vec(0x1, 385)).len(), 49); // rounds up
        assert_eq!(quantize_binary(&unit_vec(0x1, 1)).len(), 1);
    }

    #[test]
    fn hamming_score_self_is_one() {
        let bits = quantize_binary(&unit_vec(0xfeed, 384));
        assert!((hamming_score(&bits, &bits, 384) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hamming_score_inverse_is_minus_one() {
        // A vector and its negation flip every sign bit → max Hamming → -1.
        let v = unit_vec(0xabc, 384);
        let neg: Vec<f32> = v.iter().map(|x| -x).collect();
        let s = hamming_score(&quantize_binary(&v), &quantize_binary(&neg), 384);
        // -0.0 stays positive in our `>= 0.0` test, so a handful of exact-zero
        // components may not flip; allow a tiny margin.
        assert!(s < -0.99, "expected ~-1.0 for negated vector, got {s}");
    }

    #[test]
    fn hamming_score_ranks_self_above_unrelated() {
        // The ranking property the brute-force path relies on: a vector's own
        // bits score higher than an unrelated vector's bits.
        let q = unit_vec(0x111, 384);
        let other = unit_vec(0x222, 384);
        let q_bits = quantize_binary(&q);
        let self_score = hamming_score(&q_bits, &q_bits, 384);
        let other_score = hamming_score(&q_bits, &quantize_binary(&other), 384);
        assert!(
            self_score > other_score,
            "self {self_score} should outrank unrelated {other_score}"
        );
    }

    #[test]
    fn dequantize_binary_is_unit_norm_signs() {
        let v = unit_vec(0x9, 384);
        let back = dequantize_binary(&quantize_binary(&v), 384);
        assert_eq!(back.len(), 384);
        let n: f32 = back.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-4, "expected unit norm, got {n}");
        // Signs must match the original components.
        for (o, b) in v.iter().zip(&back) {
            assert_eq!(*o >= 0.0, *b >= 0.0, "sign mismatch");
        }
    }
}
