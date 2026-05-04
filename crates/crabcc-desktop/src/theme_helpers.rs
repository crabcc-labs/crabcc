//! Tone helpers shared across routes.
//!
//! Tiny module, single function today. Promoted from inline copies in
//! `routes::dashboard` and `routes::timeline` so a third caller (the
//! commands launchpad's running-row indicator, for instance) doesn't
//! create a third drift-prone duplicate.
//!
//! When this grows past one or two helpers, split per-concern into
//! `theme_helpers::activity`, `theme_helpers::agents`, etc.

use gpui::Hsla;
use gpui_component::Theme;

/// Map a tracked-activity op string (`sym` / `refs` / `callers` /
/// `fuzzy` / `prefix` / `random-query` / `ingest` / `memory.ingest` /
/// …) to a tone in the gpui-component theme. The palette is the
/// dashboard's de-facto activity-tile convention; routes that
/// surface the same op universe (Timeline inspector, K-Graph wing
/// chips eventually) reuse it for visual consistency.
///
/// Unknown / uncategorised ops fall through to `muted_foreground` so
/// they don't pull the eye — `outline`, `track`, and anything new
/// the indexer surfaces dominate row volume in a typical session.
pub fn op_color(op: &str, theme: &Theme) -> Hsla {
    match op {
        "sym" => theme.primary,
        "refs" => theme.info,
        "callers" => theme.warning,
        "fuzzy" | "prefix" | "random-query" => theme.success,
        "ingest" | "memory.ingest" => theme.primary,
        _ => theme.muted_foreground,
    }
}
