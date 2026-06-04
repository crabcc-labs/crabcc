use crate::audit::model::AuditReport;
use std::fmt::Write;

pub fn render_json(report: &AuditReport) -> anyhow::Result<String> {
    Ok(sonic_rs::to_string_pretty(report)?)
}

pub fn render_human(report: &AuditReport) -> String {
    let mut output = String::new();
    writeln!(output, "Sessions scanned: {}", report.sessions_scanned).unwrap();
    writeln!(output, "Total tokens: {}", report.total_tokens).unwrap();
    writeln!(output, "Wasted tokens: {}", report.wasted_tokens).unwrap();

    if !report.findings.is_empty() {
        writeln!(output, "\nTop waste findings:").unwrap();
        writeln!(output, "{:.<20} {:>10} {:<50}", "Kind", "Tokens", "Detail").unwrap();
        writeln!(output, "{}", "-".repeat(80)).unwrap();

        for finding in report.findings.iter().take(10) {
            let detail = if finding.detail.len() > 47 {
                format!("{}...", &finding.detail[..47])
            } else {
                finding.detail.clone()
            };
            writeln!(
                output,
                "{:.<20} {:>10} {:<50}",
                finding.kind, finding.tokens, detail
            )
            .unwrap();
        }
    }

    output
}
