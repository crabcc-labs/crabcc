use anyhow::Result;

pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed_one(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_one(t)).collect()
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
}
