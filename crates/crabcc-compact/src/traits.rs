use crate::types::*;

pub trait Reasoner: Send + Sync {
    fn analyze(&self, input: &CompactInput) -> anyhow::Result<Vec<TransformStep>>;
}

pub trait Coder: Send + Sync {
    fn apply(&self, code: &str, steps: &[TransformStep]) -> anyhow::Result<String>;
}

pub trait SmoothnessEvaluator: Send + Sync {
    fn score(&self, original: &str, compacted: &str, steps: &[TransformStep]) -> SmoothnessScore;
}
