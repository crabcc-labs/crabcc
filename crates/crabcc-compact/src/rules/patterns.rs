use crate::types::*;

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    let mut steps = Vec::new();
    for line in input.original_code.lines() {
        if !line.is_empty() && line.chars().all(|c| c == ' ' || c == '\t') {
            let ws_chars = line.len() as u32;
            steps.push(TransformStep {
                kind: TransformKind::WhitespaceComment,
                description: "Line contains only whitespace".into(),
                tokens_saved: ws_chars / 4,
            });
        }
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
    fn detects_whitespace_only_lines() {
        let code = "fn foo() {\n    \n    let x = 1;\n\t\t\n}\n";
        let steps = find(&input(code));
        assert!(!steps.is_empty(), "should detect whitespace-only lines");
        assert!(steps
            .iter()
            .all(|s| s.kind == TransformKind::WhitespaceComment));
    }

    #[test]
    fn empty_lines_not_flagged() {
        // Truly empty lines (no chars at all) are skipped
        let code = "fn foo() {\n\n    let x = 1;\n}\n";
        assert!(find(&input(code)).is_empty());
    }

    #[test]
    fn normal_code_lines_not_flagged() {
        let code = "let x = 1;\nlet y = 2;\n";
        assert!(find(&input(code)).is_empty());
    }
}
