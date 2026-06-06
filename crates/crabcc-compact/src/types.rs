use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactInput {
    pub session_id: String,
    pub original_code: String,
    pub file_type: String,
    pub project_scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactOutput {
    pub compacted_code: String,
    pub steps: Vec<TransformStep>,
    pub metrics: CompactMetrics,
    pub smoothness: SmoothnessScore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformStep {
    pub kind: TransformKind,
    pub description: String,
    pub tokens_saved: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransformKind {
    RedundancyRemoval,
    ConditionalSimplification,
    ExpressionFolding,
    DeadCodeRemoval,
    PatternNormalization,
    WhitespaceComment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactMetrics {
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub tokens_saved: u32,
    pub complexity_before: f64,
    pub complexity_after: f64,
    pub process_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmoothnessScore {
    pub disruption: f64,
    pub readability: f64,
    pub compatibility: f64,
    pub preservation: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactJsonlEntry {
    pub session_id: String,
    pub r#type: String,
    pub original_code: String,
    pub compressed_code: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactDrawerBody {
    pub original: String,
    pub compacted: String,
    pub steps: Vec<TransformStep>,
    pub metrics: CompactMetrics,
    pub smoothness: SmoothnessScore,
    pub context: CompactContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactContext {
    pub file_type: String,
    pub project_scope: Option<String>,
}
