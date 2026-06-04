//! Pure link extraction: pull crawlable `<a href>` targets out of a raw
//! HTML document and resolve them to absolute `http(s)` URLs.
//!
//! This is deliberately a lightweight attribute scan rather than a full
//! HTML parse — `htmd` already owns the heavyweight parse for *content*
//! extraction; here we only need raw `href` values and want to tolerate
//! malformed markup without pulling a second parser into the build.

use std::collections::HashSet;
use url::Url;

/// Extract absolute `http(s)` link targets from `html`, resolving
/// relative hrefs against `base`.
///
/// - Order preserved, duplicates removed.
/// - Fragments stripped, so `/p#a` and `/p#b` collapse to one target.
/// - Non-navigational schemes (`mailto:`, `javascript:`, `tel:`,
///   `data:`) and bare `#…` anchors are dropped.
/// - Returns an empty `Vec` when `base` itself is unparseable.
pub fn extract_links(base: &str, html: &str) -> Vec<String> {
    let base = match Url::parse(base) {
        Ok(u) => u,
        Err(_) => return Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in href_values(html) {
        // Real-world hrefs entity-escape query separators
        // (`/search?a=1&amp;b=2`); decode before resolving or the crawler
        // requests a corrupted `amp;b` param and misses paginated links.
        let decoded = decode_entities(raw.trim());
        let raw = decoded.as_str();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("javascript:")
            || lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("data:")
        {
            continue;
        }
        let Ok(mut abs) = base.join(raw) else {
            continue;
        };
        if !matches!(abs.scheme(), "http" | "https") {
            continue;
        }
        abs.set_fragment(None);
        let s = abs.to_string();
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// Scan for `href="…"`, `href='…'`, and unquoted `href=…` attribute
/// values. Byte-oriented so arbitrary UTF-8 in a value can't panic on a
/// non-char-boundary slice; values are recovered lossily.
fn href_values(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase(); // length-preserving (ASCII only)
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find("href") {
        let mut j = i + rel + 4; // just past "href"
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            i += rel + 4; // not an attribute assignment — keep scanning
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let (start, end, next) = match bytes[j] {
            q @ (b'"' | b'\'') => {
                let start = j + 1;
                let mut k = start;
                while k < bytes.len() && bytes[k] != q {
                    k += 1;
                }
                (start, k, (k + 1).min(bytes.len()))
            }
            _ => {
                let start = j;
                let mut k = start;
                while k < bytes.len() && !bytes[k].is_ascii_whitespace() && bytes[k] != b'>' {
                    k += 1;
                }
                (start, k, k)
            }
        };
        out.push(String::from_utf8_lossy(&bytes[start..end]).into_owned());
        i = next.max(i + rel + 4);
    }
    out
}

/// Decode the HTML entities that actually show up in `href` attributes:
/// the named five (`&amp; &lt; &gt; &quot; &apos;`) plus numeric
/// (`&#38;` / `&#x26;`). Unknown or malformed entities are left verbatim.
/// Deliberately minimal — a full entity table belongs to an HTML parser,
/// not to link harvesting.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // An entity name/number is short; cap the scan so a stray `&` in
        // prose doesn't swallow the rest of the string.
        let decoded = after
            .find(';')
            .filter(|&semi| semi > 0 && semi <= 10)
            .and_then(|semi| {
                let body = &after[..semi];
                let ch = match body {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => body
                        .strip_prefix("#x")
                        .or_else(|| body.strip_prefix("#X"))
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .or_else(|| body.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                        .and_then(char::from_u32),
                };
                ch.map(|c| (c, semi))
            });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_amp_in_query_before_resolving() {
        let links = extract_links(
            "https://s.example/",
            r#"<a href="/search?a=1&amp;b=2&amp;c=3">x</a>"#,
        );
        assert_eq!(
            links,
            vec!["https://s.example/search?a=1&b=2&c=3".to_string()]
        );
    }

    #[test]
    fn decodes_numeric_entities() {
        assert_eq!(decode_entities("a&#38;b&#x26;c"), "a&b&c");
        // Lone & and unknown entities are preserved verbatim.
        assert_eq!(
            decode_entities("rock & roll &nope; end"),
            "rock & roll &nope; end"
        );
    }

    #[test]
    fn resolves_relative_and_absolute() {
        let html = r#"
            <a href="/about">about</a>
            <a href='sub/page.html'>sub</a>
            <a href="https://other.example/x">abs</a>
            <a href="../up">up</a>
        "#;
        let links = extract_links("https://site.example/dir/index.html", html);
        assert!(links.contains(&"https://site.example/about".to_string()));
        assert!(links.contains(&"https://site.example/dir/sub/page.html".to_string()));
        assert!(links.contains(&"https://other.example/x".to_string()));
        assert!(links.contains(&"https://site.example/up".to_string()));
    }

    #[test]
    fn drops_non_navigational_and_dedups() {
        let html = r##"
            <a href="mailto:a@b.c">mail</a>
            <a href="javascript:void(0)">js</a>
            <a href="tel:+1">tel</a>
            <a href="#section">anchor</a>
            <a href="/dup">one</a>
            <a href="/dup#frag">two (same after fragment strip)</a>
        "##;
        let links = extract_links("https://site.example/", html);
        assert_eq!(links, vec!["https://site.example/dup".to_string()]);
    }

    #[test]
    fn unparseable_base_yields_nothing() {
        assert!(extract_links("not a url", "<a href=/x>").is_empty());
    }

    #[test]
    fn unquoted_href_value() {
        let links = extract_links("https://s.example/", "<a href=/raw>x</a>");
        assert_eq!(links, vec!["https://s.example/raw".to_string()]);
    }
}
