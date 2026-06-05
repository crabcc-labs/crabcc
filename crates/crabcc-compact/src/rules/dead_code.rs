use crate::types::*;

const CODE_KEYWORDS: &[&str] = &["let ", "fn ", "return ", "if ", "for ", "while ", "match "];

fn looks_like_code(line: &str) -> bool {
    CODE_KEYWORDS.iter().any(|kw| line.contains(kw))
}

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    let mut steps = Vec::new();

    for line in input.original_code.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("// ") && looks_like_code(&trimmed[3..]) {
            // Store the trimmed line as description so rewrite.rs can use it as a
            // literal filter pattern (rewrite filters lines containing description).
            steps.push(TransformStep {
                kind: TransformKind::DeadCodeRemoval,
                description: trimmed.to_string(),
                tokens_saved: trimmed.len() as u32 / 4,
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
    fn detects_commented_code_block() {
        let code = "// let x = 1;\n// return x;\n";
        let steps = find(&input(code));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].kind, TransformKind::DeadCodeRemoval);
        // description must be the actual line text so rewrite can use it as a filter
        assert_eq!(steps[0].description, "// let x = 1;");
        assert_eq!(steps[1].description, "// return x;");
    }

    #[test]
    fn consecutive_lines_each_produce_a_step() {
        let code = "// fn foo() {\n// let y = 2;\n// return y;\n// }\n";
        let steps = find(&input(code));
        // Each commented-out code line produces one step; '}' has no keyword so 3 steps
        assert_eq!(steps.len(), 3, "each commented-code line produces one step");
        // descriptions are the actual line text for use as filter patterns
        assert!(steps.iter().all(|s| s.description.starts_with("// ")));
    }

    #[test]
    fn plain_comments_not_flagged() {
        let code = "// This is a normal comment\n// explaining something\n";
        assert!(find(&input(code)).is_empty());
    }
}
