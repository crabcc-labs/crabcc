//! Reciprocal Rank Fusion — the blend layer for hybrid search.
//!
//! See [`super::Palace::search_hybrid`] for the entry point.

use crate::types::*;
use std::collections::HashMap;

/// Reciprocal Rank Fusion constant (Cormack/Clarke/Buettcher 2009). Larger
/// `k` softens the rank curve so top-of-list disagreements between rankers
/// matter less; the original paper used 60 and that's also what TREC and
/// most production hybrid-search stacks default to.
pub(super) const RRF_K: usize = 60;

/// Reciprocal Rank Fusion across N rankers. Each `ranking` is a list of
/// hits in descending-quality order; the contribution of a hit at rank `r`
/// (1-based) to the fused score is `1 / (RRF_K + r)`. Hits that appear in
/// more than one ranking accumulate score, which is how the "vote across
/// rankers" intuition emerges from the math. Output is sorted by fused
/// score descending and truncated to `limit`. The first ranker breaks ties
/// among rankings so the order is deterministic.
pub(super) fn rrf_fuse(rankings: &[&[DrawerHit]], limit: usize) -> Vec<DrawerHit> {
    if limit == 0 || rankings.iter().all(|r| r.is_empty()) {
        return Vec::new();
    }
    let mut fused: HashMap<DrawerId, (f32, DrawerHit)> = HashMap::new();
    for ranking in rankings {
        for (rank, hit) in ranking.iter().enumerate() {
            let contribution = 1.0_f32 / (RRF_K as f32 + (rank + 1) as f32);
            fused
                .entry(hit.id)
                .and_modify(|(s, _)| *s += contribution)
                .or_insert((contribution, hit.clone()));
        }
    }
    let mut out: Vec<(f32, DrawerHit)> = fused.into_values().collect();
    // Stable order: fused score desc, then drawer id asc on ties.
    out.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.id.cmp(&b.1.id))
    });
    out.truncate(limit);
    out.into_iter()
        .map(|(score, mut hit)| {
            // Surface the fused RRF score on the returned hit so callers
            // can see why hybrid ordered the list this way. Single-ranker
            // raw scores stay accessible via `search_vector` /
            // `search_lexical` directly.
            hit.score = score;
            hit
        })
        .collect()
}
