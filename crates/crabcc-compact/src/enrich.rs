pub fn detect_trigger(prompt: &str, trigger: &str) -> Option<String> {
    if trigger.is_empty() {
        return None;
    }
    let stripped = prompt.strip_prefix(trigger)?;
    Some(stripped.trim_start().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_prefix_detected_and_stripped() {
        let result = detect_trigger("!e add rate limiting to the API", "!e");
        assert_eq!(result.unwrap(), "add rate limiting to the API");
    }

    #[test]
    fn no_trigger_returns_none() {
        assert!(detect_trigger("add rate limiting to the API", "!e").is_none());
    }

    #[test]
    fn empty_trigger_returns_none() {
        assert!(detect_trigger("anything", "").is_none());
    }

    #[test]
    fn trigger_only_returns_empty_string() {
        assert_eq!(detect_trigger("!e", "!e").unwrap(), "");
    }

    #[test]
    fn custom_trigger_works() {
        let result = detect_trigger("!enrich fix the auth middleware", "!enrich");
        assert_eq!(result.unwrap(), "fix the auth middleware");
    }
}
