#[derive(Debug, Default)]
pub struct Budget {
    pub total_original: usize,
    pub total_compressed: usize,
    pub dedup_hits: usize,
    pub calls: usize,
}

impl Budget {
    pub fn new() -> Self { Self::default() }

    pub fn record_compress(&mut self, original: usize, compressed: usize) {
        self.calls += 1;
        self.total_original += original;
        self.total_compressed += compressed;
    }

    #[allow(dead_code)]
    pub fn record_dedup(&mut self, size: usize) {
        self.dedup_hits += 1;
        self.total_original += size;
    }

    pub fn tokens_saved(&self) -> usize {
        self.total_original.saturating_sub(self.total_compressed)
    }

    pub fn pressure(&self) -> f32 {
        let baseline = 100_000usize;
        (self.total_original as f32 / baseline as f32).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_budget_has_zero_pressure() {
        assert_eq!(Budget::new().pressure(), 0.0);
    }

    #[test]
    fn records_compress_and_computes_savings() {
        let mut b = Budget::new();
        b.record_compress(1000, 500);
        assert_eq!(b.tokens_saved(), 500);
        assert_eq!(b.calls, 1);
    }

    #[test]
    fn records_dedup_adds_to_original() {
        let mut b = Budget::new();
        b.record_dedup(2000);
        assert_eq!(b.total_original, 2000);
        assert_eq!(b.total_compressed, 0);
        assert_eq!(b.dedup_hits, 1);
    }

    #[test]
    fn pressure_caps_at_one() {
        let mut b = Budget::new();
        b.record_compress(200_000, 100_000);
        assert_eq!(b.pressure(), 1.0);
    }
}
