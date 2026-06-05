use crate::types::*;
use std::collections::HashMap;

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for line in input.original_code.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            *counts.entry(trimmed).or_insert(0) += 1;
        }
    }

    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(line, count)| {
            let tokens_saved = (line.len() as u32 * (count - 1)) / 4;
            TransformStep {
                kind: TransformKind::RedundancyRemoval,
                description: format!("Repeated line ({} times): {}", count, &line[..line.len().min(60)]),
                tokens_saved,
            }
        })
        .collect()
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
    fn detects_repeated_line() {
        let code = "let x = 1;\nlet y = 2;\nlet x = 1;\n";
        let steps = find(&input(code));
        assert!(!steps.is_empty(), "should detect repeated line");
        assert!(steps.iter().all(|s| s.kind == TransformKind::RedundancyRemoval));
        assert!(steps.iter().any(|s| s.tokens_saved > 0));
    }

    #[test]
    fn no_steps_for_unique_lines() {
        let code = "let x = 1;\nlet y = 2;\nlet z = 3;\n";
        assert!(find(&input(code)).is_empty());
    }
}
