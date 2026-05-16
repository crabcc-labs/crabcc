//! Search-mode enum and parsing.

use serde::{Deserialize, Serialize};

/// Which retriever(s) to use.
///
/// The `Default` impl is feature-conditional (issue #20):
///
/// - With `memory-embed` ON → `Hybrid` (vector + lexical via RRF).
///   Real semantic embeddings make the vector path informative.
/// - With `memory-embed` OFF → `Lexical` (BM25 only).
///   The default `HashEmbedder` produces deterministic-but-meaningless
///   vectors — running cosine over them is noise, so we fall back to
///   keyword search until a real embedder is plugged in.
///
/// `Vector` is exposed for ablation regardless of features, and remains
/// what callers will explicitly pick once they've wired their own
/// `Embedder` impl through `Palace::with_components`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Hybrid,
    Lexical,
    Vector,
}

// Feature-conditional default — clippy's `derivable_impls` would have us
// `#[derive(Default)]` with `#[default]` on a single variant, but the
// chosen variant flips with a cargo feature so a hand-written impl is
// the only way to express that.
#[allow(clippy::derivable_impls)]
impl Default for SearchMode {
    fn default() -> Self {
        // With no real embedder available, vector hits are meaningless,
        // so the safe default is BM25-only. Hosts that construct
        // `Palace::with_components(..., Arc<FastEmbedder>)` can still
        // ask for `Hybrid` explicitly.
        #[cfg(feature = "memory-embed")]
        {
            Self::Hybrid
        }
        #[cfg(not(feature = "memory-embed"))]
        {
            Self::Lexical
        }
    }
}

impl SearchMode {
    /// Parse the CLI / API value. Accepts the three lower-case spellings
    /// plus a couple of common aliases.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hybrid" | "rrf" | "fusion" => Some(Self::Hybrid),
            "lexical" | "fts" | "bm25" | "text" => Some(Self::Lexical),
            "vector" | "knn" | "ann" | "embed" => Some(Self::Vector),
            _ => None,
        }
    }
}
