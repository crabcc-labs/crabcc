#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionFile {
    pub path: String,
    pub project: String,
    pub messages: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WasteFinding {
    pub session: String,
    pub kind: String,
    pub detail: String,
    pub tokens: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditReport {
    pub sessions_scanned: usize,
    pub total_tokens: usize,
    pub wasted_tokens: usize,
    pub findings: Vec<WasteFinding>,
}
