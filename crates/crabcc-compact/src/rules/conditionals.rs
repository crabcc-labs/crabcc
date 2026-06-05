use crate::types::*;

const TRIVIAL_PATTERNS: &[&str] = &[
    "if true {",
    "if false {",
    "if 1 == 1 {",
    "if 0 == 0 {",
];

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    let mut steps = Vec::new();
    for line in input.original_code.lines() {
        let trimmed = line.trim();
        for pattern in TRIVIAL_PATTERNS {
            if trimmed.contains(pattern) {
                steps.push(TransformStep {
                    kind: TransformKind::ConditionalSimplification,
                    description: format!("Trivially constant condition: {}", pattern),
                    tokens_saved: pattern.len() as u32 / 4,
                });
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
    fn detects_if_true() {
        let steps = find(&input("    if true {\n        do_something();\n    }\n"));
        assert!(!steps.is_empty());
        assert_eq!(steps[0].kind, TransformKind::ConditionalSimplification);
    }

    #[test]
    fn detects_if_false() {
        let steps = find(&input("if false {\n    unreachable!();\n}\n"));
        assert!(!steps.is_empty());
    }

    #[test]
    fn detects_1_eq_1() {
        let steps = find(&input("if 1 == 1 {\n    println!(\"yes\");\n}\n"));
        assert!(!steps.is_empty());
    }

    #[test]
    fn no_steps_for_normal_conditional() {
        let steps = find(&input("if x > 0 {\n    do_something();\n}\n"));
        assert!(steps.is_empty());
    }
}
