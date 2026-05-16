//! Tiny shared time utilities. The `SystemTime::now().duration_since(UNIX_EPOCH)
//! .map(|d| d.as_secs()).unwrap_or(0)` incantation was copy-pasted in 9 places
//! across godfather/cli/memory; this is the single source of truth.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch. Returns 0 if the system clock is before
/// 1970 — a degraded-but-defined signal that callers can compare against,
/// rather than an error path nobody handles. The local fn shims that used
/// to live in 9 places (godfather::{session,event,godfather,watch,cleanup},
/// crabcc_cli::{agent_runs_db,backup}, crabcc_memory::backend::{sqlite,in_memory})
/// all collapse to this one impl.
#[inline]
pub fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_now_secs_is_post_2024() {
        // Cheap sanity — the build date should always be after 2024-01-01,
        // i.e. > 1_704_067_200. Catches "fn returns the literal 0" bugs and
        // keeps a regression net for any future replacement impl.
        assert!(
            unix_now_secs() > 1_704_067_200,
            "unix_now_secs returned a pre-2024 timestamp"
        );
    }
}
