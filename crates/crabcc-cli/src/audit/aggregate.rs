use crate::audit::model::{AuditReport, SessionFile, WasteFinding};

pub fn build_report(sessions: &[SessionFile], findings: Vec<WasteFinding>) -> AuditReport {
    let sessions_scanned = sessions.len();
    let total_tokens = sessions.iter().map(|s| s.total_tokens).sum();
    let wasted_tokens = findings.iter().map(|f| f.tokens).sum();

    let mut sorted_findings = findings;
    sorted_findings.sort_unstable_by_key(|b| std::cmp::Reverse(b.tokens));

    AuditReport {
        sessions_scanned,
        total_tokens,
        wasted_tokens,
        findings: sorted_findings,
    }
}
