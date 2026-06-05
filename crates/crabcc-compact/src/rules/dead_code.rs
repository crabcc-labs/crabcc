use crate::types::*;

const CODE_KEYWORDS: &[&str] = &["let ", "fn ", "return ", "if ", "for ", "while ", "match "];

fn looks_like_code(line: &str) -> bool {
    CODE_KEYWORDS.iter().any(|kw| line.contains(kw))
}

fn flush_cluster(cluster_size: u32, cluster_tokens: u32, steps: &mut Vec<TransformStep>) {
    if cluster_size > 0 {
        steps.push(TransformStep {
            kind: TransformKind::DeadCodeRemoval,
            description: format!("Commented-out code block ({} lines)", cluster_size),
            tokens_saved: cluster_tokens,
        });
    }
}

pub fn find(input: &CompactInput) -> Vec<TransformStep> {
    let mut steps = Vec::new();
    let mut cluster_size: u32 = 0;
    let mut cluster_tokens: u32 = 0;

    for line in input.original_code.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("// ") && looks_like_code(&trimmed[3..]) {
            cluster_size += 1;
            cluster_tokens += trimmed.len() as u32 / 4;
        } else {
            flush_cluster(cluster_size, cluster_tokens, &mut steps);
            cluster_size = 0;
            cluster_tokens = 0;
        }
    }
    flush_cluster(cluster_size, cluster_tokens, &mut steps);

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
        assert!(!steps.is_empty());
        assert_eq!(steps[0].kind, TransformKind::DeadCodeRemoval);
    }

    #[test]
    fn clusters_consecutive_lines() {
        let code = "// fn foo() {\n// let y = 2;\n// return y;\n// }\n";
        let steps = find(&input(code));
        assert_eq!(steps.len(), 1, "consecutive commented-code lines = one cluster");
    }

    #[test]
    fn plain_comments_not_flagged() {
        let code = "// This is a normal comment\n// explaining something\n";
        assert!(find(&input(code)).is_empty());
    }
}
