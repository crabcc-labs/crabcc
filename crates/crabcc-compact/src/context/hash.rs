use ahash::AHashSet;

pub struct SessionCache {
    seen: AHashSet<u64>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self { seen: AHashSet::new() }
    }

    pub fn is_seen(&self, text: &str) -> bool {
        self.seen.contains(&hash(text))
    }

    pub fn mark_seen(&mut self, text: &str) {
        self.seen.insert(hash(text));
    }
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_text_not_seen() {
        let cache = SessionCache::new();
        assert!(!cache.is_seen("hello world"));
    }

    #[test]
    fn marked_text_is_seen() {
        let mut cache = SessionCache::new();
        cache.mark_seen("hello world");
        assert!(cache.is_seen("hello world"));
    }

    #[test]
    fn different_text_not_seen() {
        let mut cache = SessionCache::new();
        cache.mark_seen("hello world");
        assert!(!cache.is_seen("hello worlds"));
    }

    #[test]
    fn empty_string_dedups() {
        let mut cache = SessionCache::new();
        cache.mark_seen("");
        assert!(cache.is_seen(""));
    }
}
