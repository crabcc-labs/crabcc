//! Query-string parsing for the `/api/graph` endpoint, plus a minimal
//! percent-decoder used by every handler that reads URL params.

use anyhow::Result;

pub(crate) struct Query {
    pub root: String,
    pub dir: String,
    pub depth: usize,
}

pub(crate) fn parse_query(raw: &str) -> Result<Query> {
    let mut root = None;
    let mut dir = String::from("callers");
    let mut depth = 2usize;
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => (pair, ""),
        };
        let v = url_decode(v);
        match k {
            "root" => root = Some(v),
            "dir" => {
                if v == "callers" || v == "callees" {
                    dir = v;
                } else {
                    anyhow::bail!("dir must be 'callers' or 'callees'");
                }
            }
            "depth" => {
                depth = v
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("depth must be a non-negative integer"))?;
            }
            _ => {}
        }
    }
    let root = root.ok_or_else(|| anyhow::anyhow!("missing required parameter: root"))?;
    if root.is_empty() {
        anyhow::bail!("root must be non-empty");
    }
    Ok(Query { root, dir, depth })
}

/// Minimal percent-decoder for query-string values. We only accept ASCII
/// printable identifiers + a few separators here, so a hand-rolled decoder
/// avoids pulling in a urlencoding crate just for this one call site.
pub(crate) fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &bytes[i + 1..i + 3];
                if let (Some(h), Some(l)) = (hex_digit(hex[0]), hex_digit(hex[1])) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
