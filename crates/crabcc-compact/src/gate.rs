pub fn token_estimate(text: &str) -> usize {
    text.len() / 4
}

pub fn above_threshold(text: &str, threshold: usize) -> bool {
    token_estimate(text) > threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_short_text() {
        let text = "a".repeat(40);
        assert_eq!(token_estimate(&text), 10);
    }

    #[test]
    fn below_threshold_returns_false() {
        let text = "a".repeat(400);
        assert!(!above_threshold(&text, 200));
    }

    #[test]
    fn above_threshold_returns_true() {
        let text = "a".repeat(8200);
        assert!(above_threshold(&text, 2000));
    }

    #[test]
    fn empty_text_below_any_threshold() {
        assert!(!above_threshold("", 1));
    }
}
