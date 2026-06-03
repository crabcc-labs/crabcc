use crate::audit::{model, tokens};
use std::collections::HashMap;

pub fn analyze(session: &model::SessionFile, raw_lines: &[String]) -> Vec<model::WasteFinding> {
    let mut findings = Vec::new();
    let mut line_counts = HashMap::new();

    for line in raw_lines {
        // Check for huge tool output
        let token_count = tokens::estimate_tokens(line);
        if token_count > 10000 {
            findings.push(model::WasteFinding {
                session: session.path.clone(),
                kind: "huge_tool_output".to_string(),
                detail: format!("Line with {} estimated tokens", token_count),
                tokens: token_count,
            });
        }

        // Count line occurrences for duplicate detection
        *line_counts.entry(line).or_insert(0) += 1;
    }

    // Check for duplicate reads
    for (line, count) in line_counts {
        if count >= 3 {
            let token_count = tokens::estimate_tokens(line);
            let wasted_tokens = token_count * (count - 1);
            findings.push(model::WasteFinding {
                session: session.path.clone(),
                kind: "duplicate_read".to_string(),
                detail: format!("Line repeated {} times", count),
                tokens: wasted_tokens,
            });
        }
    }

    findings
}
