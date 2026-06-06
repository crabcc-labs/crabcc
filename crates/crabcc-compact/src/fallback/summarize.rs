#[allow(dead_code)]
pub fn extract_errors(text: &str) -> String {
    let error_patterns = [
        "error",
        "Error",
        "ERROR",
        "warning",
        "Warning",
        "WARN",
        "panic",
        "PANIC",
        "failed",
        "FAILED",
        "exception",
        "Exception",
    ];
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| error_patterns.iter().any(|p| l.contains(p)))
        .collect();
    if lines.is_empty() {
        text.lines().take(20).collect::<Vec<_>>().join(
            "
",
        )
    } else {
        lines.join(
            "
",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_error_lines() {
        let text = "ok line
error: something broke
another ok
warning: watch out
fine";
        let result = extract_errors(text);
        assert!(result.contains("error: something broke"));
        assert!(result.contains("warning: watch out"));
        assert!(!result.contains("ok line"));
    }

    #[test]
    fn no_errors_returns_first_20_lines() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let text = lines.join(
            "
",
        );
        let result = extract_errors(&text);
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(result_lines.len(), 20);
    }
}
