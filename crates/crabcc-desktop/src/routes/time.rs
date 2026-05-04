//! Tiny time-formatting helpers shared across routes.
//!
//! Two routes (logs, timeline) carried byte-for-byte copies of the
//! `HH:MM:SS` formatter. Promoted to a shared module so a third
//! caller doesn't fork a third copy. Per-route relative-age helpers
//! (`relative_age` in agents, `fmt_age_short` in dashboard,
//! `format_relative` in knowledge) intentionally stay separate —
//! their clock-proxy semantics differ enough that consolidating
//! would just push the per-call wrapper noise into a generic helper.

/// `HH:MM:SS` time-of-day from a unix-seconds timestamp. UTC;
/// formatting in the user's local zone needs a date crate
/// (`chrono` / `time`) and isn't worth the dep weight for a
/// developer-facing log tail / timeline list.
pub fn format_time(unix_seconds: i64) -> String {
    let day = unix_seconds.rem_euclid(86_400);
    let h = day / 3600;
    let m = (day / 60) % 60;
    let s = day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_time_pads_components() {
        // 1970-01-01 00:00:01
        assert_eq!(format_time(1), "00:00:01");
        // 1970-01-01 01:02:03 (1 * 3600 + 2 * 60 + 3 = 3723)
        assert_eq!(format_time(3723), "01:02:03");
        // Top of midnight
        assert_eq!(format_time(0), "00:00:00");
    }

    #[test]
    fn format_time_wraps_across_days() {
        // 23:59:59 of any day is the same
        assert_eq!(format_time(86_399), "23:59:59");
        // Day rollover — second after midnight
        assert_eq!(format_time(86_401), "00:00:01");
    }

    #[test]
    fn format_time_handles_negative_via_rem_euclid() {
        // Pre-epoch shouldn't happen in practice, but the formatter
        // shouldn't panic. `rem_euclid` keeps the result in [0, 86_400).
        let s = format_time(-1);
        assert_eq!(s, "23:59:59");
    }
}
