use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        })
}

/// Wrap a query result body with a SHA-256 fingerprint envelope, so an
/// agent on the other end can cache results and skip re-reading
/// identical responses.
///
/// Behaviour:
/// - `if_changed = None` → return `body` verbatim (no envelope, no
///   fingerprint cost paid). This keeps the default surface unchanged.
/// - `if_changed = Some(prev)` and `sha256(body) == prev` →
///   `{"unchanged":true,"fingerprint":"<prev>"}` so the agent can
///   reuse its cached result.
/// - `if_changed = Some(prev)` and the fingerprint differs →
///   `{"fingerprint":"<new>","result":<body>}` so the agent learns the
///   new fingerprint AND gets the fresh data in one round-trip.
///
/// `body` MUST already be valid JSON — we splice it into the envelope
/// without re-parsing.
pub fn fingerprint_envelope(body: &str, if_changed: Option<&str>) -> String {
    let Some(prev) = if_changed else {
        return body.to_string();
    };
    let fp = sha256_hex(body.as_bytes());
    if fp == prev {
        format!(r#"{{"unchanged":true,"fingerprint":"{fp}"}}"#)
    } else {
        format!(r#"{{"fingerprint":"{fp}","result":{body}}}"#)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn deterministic() {
        let a = sha256_hex(b"crabcc");
        let b = sha256_hex(b"crabcc");
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_envelope_passthrough_when_no_flag() {
        // Without --if-changed, the body is returned verbatim — the
        // default CLI surface stays byte-identical for callers that
        // don't opt in.
        let body = r#"{"hits":[]}"#;
        assert_eq!(fingerprint_envelope(body, None), body);
    }

    #[test]
    fn fingerprint_envelope_returns_unchanged_on_match() {
        let body = r#"{"hits":[{"file":"a.rs","line":1}]}"#;
        let fp = sha256_hex(body.as_bytes());
        let out = fingerprint_envelope(body, Some(&fp));
        assert_eq!(out, format!(r#"{{"unchanged":true,"fingerprint":"{fp}"}}"#));
    }

    #[test]
    fn fingerprint_envelope_wraps_on_mismatch() {
        let body = r#"{"count":3}"#;
        let stale = "0000000000000000000000000000000000000000000000000000000000000000";
        let out = fingerprint_envelope(body, Some(stale));
        // Must contain the body inline AND the fresh fingerprint AND
        // open with the envelope key in a known order — agents parse
        // this with serde_json or equivalent so we just spot-check the
        // structural contract, not byte-identical formatting.
        let v: serde_json::Value = serde_json::from_str(&out).expect("envelope is valid JSON");
        assert!(v.get("fingerprint").and_then(|f| f.as_str()).is_some());
        assert_eq!(v["result"]["count"], 3);
        assert_ne!(v["fingerprint"], stale);
    }

    #[test]
    fn fingerprint_envelope_is_round_trippable() {
        // First call: no flag, gets body back.
        let body = r#"{"x":1}"#;
        let first = fingerprint_envelope(body, None);
        assert_eq!(first, body);

        // Agent computes fp themselves and passes on next call. Body
        // is unchanged → unchanged sentinel comes back.
        let fp = sha256_hex(first.as_bytes());
        let second = fingerprint_envelope(body, Some(&fp));
        assert!(second.contains(r#""unchanged":true"#));
    }
}
