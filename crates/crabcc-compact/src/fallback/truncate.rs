pub fn truncate(text: &str, head_lines: usize, tail_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total <= head_lines + tail_lines {
        return text.to_string();
    }
    let head = &lines[..head_lines];
    let tail = &lines[total - tail_lines..];
    let omitted = total - head_lines - tail_lines;
    format!(
        "{}
... [{omitted} lines omitted by crabcc-compact fallback] ...
{}",
        head.join(
            "
"
        ),
        tail.join(
            "
"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returned_verbatim() {
        let text = "a
b
c";
        assert_eq!(truncate(text, 10, 10), text);
    }

    #[test]
    fn long_text_gets_head_and_tail() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let text = lines.join(
            "
",
        );
        let result = truncate(&text, 5, 5);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 4"));
        assert!(result.contains("line 95"));
        assert!(result.contains("line 99"));
        assert!(result.contains("90 lines omitted"));
        assert!(!result.contains("line 50"));
    }

    #[test]
    fn exactly_at_boundary_returned_verbatim() {
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let text = lines.join(
            "
",
        );
        assert_eq!(truncate(&text, 10, 10), text);
    }
}
