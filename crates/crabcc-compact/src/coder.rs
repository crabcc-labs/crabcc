use crate::{traits::Coder, transforms::rewrite, types::*};

pub struct RuleBasedCoder;

impl Coder for RuleBasedCoder {
    fn apply(&self, code: &str, steps: &[TransformStep]) -> anyhow::Result<String> {
        let mut current = code.to_string();
        for step in steps {
            match rewrite::apply_step(&current, step) {
                Ok(next) => current = next,
                // If one step fails, log and continue with the current state unchanged.
                Err(e) => {
                    eprintln!("[crabcc-compact] transform {:?} failed: {e}", step.kind);
                }
            }
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Coder;

    #[test]
    fn round_trip_duplicate_lines_reduced() {
        let coder = RuleBasedCoder;
        let code = "fn foo() {\n    let x = 1;\n    let x = 1;\n    x\n}";
        let steps = vec![TransformStep {
            kind: TransformKind::RedundancyRemoval,
            description: String::new(),
            tokens_saved: 0,
        }];
        let out = coder.apply(code, &steps).unwrap();
        assert!(out.lines().count() < code.lines().count());
        assert!(out.contains("let x = 1;"));
    }

    #[test]
    fn whitespace_comment_step_removes_blank_lines() {
        let coder = RuleBasedCoder;
        let code = "a\n   \nb\n\nc";
        let steps = vec![TransformStep {
            kind: TransformKind::WhitespaceComment,
            description: String::new(),
            tokens_saved: 0,
        }];
        let out = coder.apply(code, &steps).unwrap();
        assert!(!out.lines().any(|l| l.trim().is_empty()));
    }

    #[test]
    fn failed_step_does_not_abort() {
        // DeadCodeRemoval with an empty pattern will match every line — not an error,
        // but we verify multi-step execution continues after a no-op step.
        let coder = RuleBasedCoder;
        let code = "a\nb";
        let steps = vec![
            TransformStep {
                kind: TransformKind::PatternNormalization,
                description: String::new(),
                tokens_saved: 0,
            },
            TransformStep {
                kind: TransformKind::RedundancyRemoval,
                description: String::new(),
                tokens_saved: 0,
            },
        ];
        let out = coder.apply(code, &steps).unwrap();
        assert_eq!(out, "a\nb");
    }
}
