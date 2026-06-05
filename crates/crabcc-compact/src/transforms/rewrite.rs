use crate::types::*;

pub fn apply_step(code: &str, step: &TransformStep) -> anyhow::Result<String> {
    match step.kind {
        TransformKind::WhitespaceComment => {
            let result = code
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            Ok(result)
        }

        TransformKind::RedundancyRemoval => {
            let lines: Vec<&str> = code.lines().collect();
            let mut out: Vec<&str> = Vec::with_capacity(lines.len());
            for line in &lines {
                if out.last() != Some(line) {
                    out.push(line);
                }
            }
            Ok(out.join("\n"))
        }

        TransformKind::DeadCodeRemoval => {
            // step.description holds the literal pattern to remove
            let pattern = step.description.as_str();
            let result = code
                .lines()
                .filter(|l| !l.contains(pattern))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(result)
        }

        TransformKind::ExpressionFolding => {
            // Only literal string patterns — no eval, no AST
            let result = code
                .replace("1 + 0", "1")
                .replace("0 + 1", "1")
                .replace("x * 1", "x")
                .replace("1 * x", "x")
                .replace("x + 0", "x")
                .replace("0 + x", "x")
                .replace("x - 0", "x")
                .replace("x / 1", "x");
            Ok(result)
        }

        TransformKind::ConditionalSimplification => {
            // Full implementation requires an AST to safely rewrite control flow.
            // Returning code unchanged until AST-level support is added.
            Ok(code.to_string())
        }

        TransformKind::PatternNormalization => {
            // Collapse 3+ consecutive blank lines into 2
            let mut result = String::with_capacity(code.len());
            let mut blank_run = 0usize;
            for line in code.lines() {
                if line.trim().is_empty() {
                    blank_run += 1;
                    if blank_run <= 2 {
                        result.push('\n');
                    }
                } else {
                    blank_run = 0;
                    result.push_str(line);
                    result.push('\n');
                }
            }
            // Trim trailing newline added by the loop to match join("\n") style
            if result.ends_with('\n') {
                result.pop();
            }
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(kind: TransformKind) -> TransformStep {
        TransformStep {
            kind,
            description: String::new(),
            tokens_saved: 0,
        }
    }

    fn step_desc(kind: TransformKind, desc: &str) -> TransformStep {
        TransformStep {
            kind,
            description: desc.to_string(),
            tokens_saved: 0,
        }
    }

    #[test]
    fn whitespace_comment_removes_blank_lines() {
        let code = "fn foo() {\n    \n    let x = 1;\n\n    x\n}";
        let out = apply_step(code, &step(TransformKind::WhitespaceComment)).unwrap();
        assert!(!out.lines().any(|l| l.trim().is_empty()));
        assert!(out.contains("let x = 1;"));
    }

    #[test]
    fn redundancy_removal_deduplicates_consecutive() {
        let code = "a\na\nb\nb\nb\na";
        let out = apply_step(code, &step(TransformKind::RedundancyRemoval)).unwrap();
        assert_eq!(out, "a\nb\na");
    }

    #[test]
    fn dead_code_removal_filters_pattern() {
        let code = "let x = 1;\n// TODO: remove me\nlet y = 2;";
        let out = apply_step(
            code,
            &step_desc(TransformKind::DeadCodeRemoval, "TODO: remove me"),
        )
        .unwrap();
        assert!(!out.contains("TODO: remove me"));
        assert!(out.contains("let x = 1;"));
        assert!(out.contains("let y = 2;"));
    }

    #[test]
    fn expression_folding_replaces_literals() {
        let code = "let a = 1 + 0;\nlet b = x * 1;\nlet c = x + 0;";
        let out = apply_step(code, &step(TransformKind::ExpressionFolding)).unwrap();
        assert_eq!(out, "let a = 1;\nlet b = x;\nlet c = x;");
    }

    #[test]
    fn expression_folding_replaces_all_patterns() {
        // All 8 patterns from FOLDABLE_PATTERNS must be handled by the rewrite.
        let code = "let a = 0 + 1;\nlet b = 1 * x;\nlet c = 0 + x;\nlet d = x - 0;\nlet e = x / 1;";
        let out = apply_step(code, &step(TransformKind::ExpressionFolding)).unwrap();
        assert_eq!(
            out,
            "let a = 1;\nlet b = x;\nlet c = x;\nlet d = x;\nlet e = x;"
        );
    }

    #[test]
    fn conditional_simplification_is_passthrough() {
        let code = "if true {\n    do_thing();\n}";
        let out = apply_step(code, &step(TransformKind::ConditionalSimplification)).unwrap();
        assert_eq!(out, code);
    }

    #[test]
    fn pattern_normalization_collapses_excess_blank_lines() {
        let code = "a\n\n\n\nb";
        let out = apply_step(code, &step(TransformKind::PatternNormalization)).unwrap();
        // 4 blank lines between a and b should be collapsed to at most 2
        let blank_runs: Vec<usize> = {
            let mut runs = Vec::new();
            let mut run = 0usize;
            for line in out.lines() {
                if line.trim().is_empty() {
                    run += 1;
                } else {
                    if run > 0 {
                        runs.push(run);
                        run = 0;
                    }
                }
            }
            runs
        };
        assert!(blank_runs.iter().all(|&r| r <= 2));
    }
}
