use crate::types::*;
use anyhow::Result;

pub mod in_memory;
pub mod sqlite;

/// Storage trait. M0.5 will add a `SqliteVecBackend` impl that uses
/// the `sqlite-vec` extension for fast ANN; the trait surface stays the
/// same, callers just see lower latency on `query`.
pub trait Backend: Send + Sync {
    fn add(&self, drawers: &[DrawerInsert]) -> Result<Vec<DrawerId>>;
    fn query(&self, q: &Query) -> Result<QueryResult>;
    fn get(&self, ids: &[DrawerId]) -> Result<GetResult>;
    fn delete(&self, sel: &DeleteSel) -> Result<usize>;
    fn count(&self) -> Result<usize>;
    fn health(&self) -> HealthStatus;
}

/// L2-cosine similarity. Returns 0.0 for length-mismatched or zero vectors.
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
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
}
