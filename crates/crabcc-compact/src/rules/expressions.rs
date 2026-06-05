use crate::types::*;

const FOLDABLE_PATTERNS: &[&str] = &[
    "1 + 0",
    "0 + 1",
    "x * 1",
    "1 * x",
    "x + 0",
    "0 + x",
    "x - 0",
    "x / 1",
];

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    let mut steps = Vec::new();
    for line in input.original_code.lines() {
        for pattern in FOLDABLE_PATTERNS {
            if line.contains(pattern) {
                steps.push(TransformStep {
                    kind: TransformKind::ExpressionFolding,
                    description: format!("Constant-foldable expression: {}", pattern),
                    tokens_saved: pattern.len() as u32 / 4,
                });
                // one step per line, take first match
                break;
            }
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
    fn detects_add_zero() {
        let steps = find(&input("let y = x + 0;\n"));
        assert!(!steps.is_empty());
        assert_eq!(steps[0].kind, TransformKind::ExpressionFolding);
    }

    #[test]
    fn detects_mul_one() {
        let steps = find(&input("let z = x * 1;\n"));
        assert!(!steps.is_empty());
    }

    #[test]
    fn detects_add_one_plus_zero() {
        let steps = find(&input("let a = 1 + 0;\n"));
        assert!(!steps.is_empty());
    }

    #[test]
    fn no_steps_for_normal_expr() {
        let steps = find(&input("let a = x + y;\n"));
        assert!(steps.is_empty());
    }
}
