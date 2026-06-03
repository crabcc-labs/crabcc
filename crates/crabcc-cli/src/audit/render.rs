use crate::audit::model::AuditReport;

pub fn render_json(report: &AuditReport) -> anyhow::Result<String> {
    Ok(sonic_rs::to_string_pretty(report)?)
}

pub fn render_human(report: &AuditReport) -> String {
    let mut output = String::new();

    // Summary
    output.push_str(&format!("Sessions scanned: {}\n", report.sessions_scanned));
    output.push_str(&format!("Total tokens: {}\n", report.total_tokens));
    output.push_str(&format!("Wasted tokens: {}\n", report.wasted_tokens));

    if !report.findings.is_empty() {
        output.push_str("\nTop waste findings:\n");

        // Header
        output.push_str(&format!(
            "{:.<20} {:>10} {:<50}\n",
            "Kind", "Tokens", "Detail"
        ));
        output.push_str(&"-".repeat(80));
        output.push('\n');

        // Top findings (up to 10)
        for finding in report.findings.iter().take(10) {
            let detail = if finding.detail.len() > 47 {
                format!("{}...", &finding.detail[..47])
            } else {
                finding.detail.clone()
            };

            output.push_str(&format!(
                "{:.<20} {:>10} {:<50}\n",
                finding.kind, finding.tokens, detail
            ));
        }
    }

    output
}
