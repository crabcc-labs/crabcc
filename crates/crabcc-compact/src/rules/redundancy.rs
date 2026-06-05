use crate::types::*;

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    // Only detect consecutive identical non-empty lines, matching the rewrite's behavior.
    let mut steps = Vec::new();
    let mut prev: Option<&str> = None;
    let mut run_len: u32 = 1;

    for line in input.original_code.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            prev = None;
            run_len = 1;
            continue;
        }
        if prev == Some(trimmed) {
            run_len += 1;
        } else {
            if run_len > 1 {
                let tokens_saved = (prev.unwrap().len() as u32 * (run_len - 1)) / 4;
                steps.push(TransformStep {
                    kind: TransformKind::RedundancyRemoval,
                    description: format!(
                        "Repeated line ({} times): {}",
                        run_len,
                        &prev.unwrap()[..prev.unwrap().len().min(60)]
                    ),
                    tokens_saved,
                });
            }
            prev = Some(trimmed);
            run_len = 1;
        }
    }
    if run_len > 1 {
        let tokens_saved = (prev.unwrap().len() as u32 * (run_len - 1)) / 4;
        steps.push(TransformStep {
            kind: TransformKind::RedundancyRemoval,
            description: format!(
                "Repeated line ({} times): {}",
                run_len,
                &prev.unwrap()[..prev.unwrap().len().min(60)]
            ),
            tokens_saved,
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(code: &str) -> CompactInput {
        CompactInput {
            session_id: "test".into(),
            original_code: code.into(),
            file_type: "rs".into(),
            project_scope: None,
        }
    }

    #[test]
    fn detects_consecutive_repeated_lines() {
        // Only consecutive duplicates are detected (aligns with rewrite behavior).
        let code = "let x = 1;\nlet x = 1;\nlet y = 2;\n";
        let steps = find(&input(code));
        assert!(!steps.is_empty(), "should detect consecutive repeated line");
        assert!(steps.iter().all(|s| s.kind == TransformKind::RedundancyRemoval));
        assert!(steps.iter().any(|s| s.tokens_saved > 0));
    }

    #[test]
    fn non_consecutive_repeated_lines_not_detected() {
        // Non-consecutive duplicates are NOT detected to stay in sync with the rewrite.
        let code = "let x = 1;\nlet y = 2;\nlet x = 1;\n";
        let steps = find(&input(code));
        assert!(steps.is_empty(), "non-consecutive duplicates must not be reported");
    }

    #[test]
    fn no_steps_for_unique_lines() {
        let code = "let x = 1;\nlet y = 2;\nlet z = 3;\n";
        assert!(find(&input(code)).is_empty());
    }
}
