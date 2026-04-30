//! CommonMark parser + drawer-body sanitizer (issue #54).
//!
//! Wraps [`markdown`](https://docs.rs/markdown) (wooorm/markdown-rs) — the
//! workspace-standard markdown crate. Avoid `pulldown-cmark` and `comrak`;
//! a `cargo deny` rule enforces this at CI time.
//!
//! ## Public surface
//!
//! - [`parse`] — string → mdast (CommonMark options). Returns
//!   `anyhow::Result<Node>` rather than the upstream `Result<Node, String>`
//!   so callers compose with `?` against the rest of `crabcc-core`.
//! - [`sanitize_drawer_body`] — strip markdown syntax tokens (code-fence
//!   backticks, heading `#` markers, list bullets, link/image brackets)
//!   while preserving the *content*. Used by `Palace::remember_in_session`
//!   to clean drawer bodies before they hit FTS5 / BM25 ranking — a
//!   drawer copied from a chat transcript shouldn't have ` ``` ` or `###`
//!   appearing as searchable tokens.
//!
//! Gated behind `crabcc-core`'s `markdown` cargo feature so default
//! builds carry zero markdown deps.

use anyhow::{Context, Result};
use markdown::mdast::Node;
use markdown::{to_mdast, ParseOptions};

/// Parse a markdown string into an mdast `Node` tree using GFM rules
/// (CommonMark + tables + strikethrough + task-list + autolinks).
///
/// We pick GFM here rather than vanilla CommonMark because drawer
/// bodies routinely arrive from agent chat transcripts, README
/// fragments, and PR comments — all of which use GFM features.
/// Vanilla CommonMark would leak `~~` markers into BM25 tokens.
///
/// Errors propagated from `markdown::to_mdast` (which returns
/// `Result<Node, String>`) are wrapped as `anyhow::Error` so callers
/// compose with the rest of `crabcc-core`.
pub fn parse(input: &str) -> Result<Node> {
    to_mdast(input, &ParseOptions::gfm())
        .map_err(|e| anyhow::anyhow!("markdown parse: {e}"))
        .context("md::parse")
}

/// Strip markdown syntax tokens from `input`, preserving the textual
/// content. The output is plain text suitable for keyword indexing.
///
/// Behaviour by node kind:
/// - **Paragraphs / text** — emitted verbatim.
/// - **Headings** — emitted as plain text on their own line, no `#` markers.
/// - **Lists** — items emitted as plain lines, no bullets / `1.` prefixes.
/// - **Code blocks (fenced or indented)** — code text emitted verbatim,
///   no backtick fences or language tags. Improves BM25 hits on
///   identifiers that would otherwise be lost in a fence wrapper.
/// - **Inline code** — emitted verbatim, no backticks.
/// - **Links / images** — link text emitted; URL dropped.
/// - **Blockquotes / hr / break** — content emitted; markers dropped.
/// - **Emphasis / strong / strikethrough** — content emitted; markers dropped.
///
/// Falls back to the input string verbatim if parsing fails — better
/// than panicking on a partially-invalid drawer body. Callers that
/// need to surface parse errors should call [`parse`] directly.
pub fn sanitize_drawer_body(input: &str) -> String {
    let Ok(tree) = parse(input) else {
        return input.to_string();
    };
    let mut out = String::with_capacity(input.len());
    walk_node(&tree, &mut out);
    // Collapse runs of blank lines (the walk inserts blank separators
    // between block-level nodes; multiple blocks in a row would
    // otherwise stack 3+ newlines).
    collapse_blank_lines(&out)
}

fn walk_node(node: &Node, out: &mut String) {
    match node {
        // Block-level nodes: walk children, then emit a blank-line
        // separator so paragraphs stay on their own lines.
        Node::Root(r) => {
            for c in &r.children {
                walk_node(c, out);
            }
        }
        Node::Paragraph(p) => {
            for c in &p.children {
                walk_node(c, out);
            }
            out.push('\n');
        }
        Node::Heading(h) => {
            for c in &h.children {
                walk_node(c, out);
            }
            out.push('\n');
        }
        Node::List(l) => {
            for c in &l.children {
                walk_node(c, out);
            }
        }
        Node::ListItem(li) => {
            for c in &li.children {
                walk_node(c, out);
            }
        }
        Node::Blockquote(b) => {
            for c in &b.children {
                walk_node(c, out);
            }
        }
        Node::Code(c) => {
            // Fenced or indented code block. Emit the code text on its
            // own line; drop the language tag since BM25 doesn't need
            // "rust" / "ts" appearing as a separate token.
            out.push_str(&c.value);
            out.push('\n');
        }
        Node::InlineCode(c) => {
            out.push_str(&c.value);
        }
        Node::Text(t) => {
            out.push_str(&t.value);
        }
        Node::Emphasis(e) => {
            for c in &e.children {
                walk_node(c, out);
            }
        }
        Node::Strong(s) => {
            for c in &s.children {
                walk_node(c, out);
            }
        }
        Node::Delete(d) => {
            for c in &d.children {
                walk_node(c, out);
            }
        }
        Node::Link(l) => {
            // Link text only; drop the URL — searchable tokens come
            // from the visible text, not the href.
            for c in &l.children {
                walk_node(c, out);
            }
        }
        Node::Image(i) => {
            // Alt text is the searchable surface for an image.
            out.push_str(&i.alt);
        }
        Node::Break(_) => out.push('\n'),
        Node::ThematicBreak(_) => out.push('\n'),
        // Tables, footnotes, MDX nodes etc. — fall through and recurse
        // by best-effort. The mdast `Node` enum exposes children only
        // on container variants, so anything we don't explicitly
        // handle just contributes its top-level text content (if any).
        other => {
            if let Some(kids) = other.children() {
                for c in kids {
                    walk_node(c, out);
                }
            }
        }
    }
}

/// Squash any run of 2+ newlines down to a single blank line. The
/// walk emits paragraph-level newlines liberally; this normalises
/// the output so token positions stay predictable for ranking.
fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newline_run = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            newline_run += 1;
            // Allow at most two consecutive newlines (= one blank line).
            if newline_run <= 2 {
                out.push(ch);
            }
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    // Trim trailing whitespace so the output ends without a dangling
    // blank line — keeps assertions in tests readable.
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- CommonMark spec compliance — small representative set ---------

    #[test]
    fn parse_paragraph() {
        let n = parse("hello world").unwrap();
        // Just confirm parse succeeded and produced a Root with a
        // Paragraph child — full mdast assertions are upstream's job.
        match n {
            Node::Root(r) => assert!(matches!(r.children[0], Node::Paragraph(_))),
            _ => panic!("expected Root"),
        }
    }

    #[test]
    fn sanitize_strips_atx_heading_marker() {
        let s = sanitize_drawer_body("## Section A");
        assert!(!s.contains('#'), "got: {s:?}");
        assert!(s.contains("Section A"), "got: {s:?}");
    }

    #[test]
    fn sanitize_unwraps_fenced_code_block() {
        // The fence backticks and the language tag must drop. The code
        // body itself is preserved so BM25 can index `Store::open`.
        let s = sanitize_drawer_body("intro\n\n```rust\nlet x = Store::open(path)?;\n```\n\noutro");
        assert!(!s.contains("```"), "fences leaked: {s:?}");
        assert!(!s.contains("rust\n"), "lang tag leaked: {s:?}");
        assert!(s.contains("Store::open(path)"), "code dropped: {s:?}");
        assert!(s.contains("intro"));
        assert!(s.contains("outro"));
    }

    #[test]
    fn sanitize_unwraps_inline_code() {
        let s = sanitize_drawer_body("call `Store::open` to start");
        assert!(!s.contains('`'), "got: {s:?}");
        assert!(s.contains("Store::open"));
    }

    #[test]
    fn sanitize_strips_list_bullets() {
        let s = sanitize_drawer_body("- alpha\n- beta\n- gamma");
        assert!(!s.contains("- "), "bullet leaked: {s:?}");
        assert!(s.contains("alpha"));
        assert!(s.contains("beta"));
        assert!(s.contains("gamma"));
    }

    #[test]
    fn sanitize_keeps_link_text_drops_url() {
        let s = sanitize_drawer_body("see [docs](https://example.com/x)");
        assert!(!s.contains("https://example.com"), "url leaked: {s:?}");
        assert!(!s.contains("[docs]"), "brackets leaked: {s:?}");
        assert!(s.contains("docs"), "text dropped: {s:?}");
    }

    #[test]
    fn sanitize_keeps_image_alt_drops_url() {
        let s = sanitize_drawer_body("![Memory diagram](https://x.png)");
        assert!(s.contains("Memory diagram"), "alt dropped: {s:?}");
        assert!(!s.contains("https://"), "url leaked: {s:?}");
    }

    #[test]
    fn sanitize_strips_emphasis_and_strong() {
        let s = sanitize_drawer_body("*important* AND **critical** AND ~~deleted~~");
        // Markers dropped; content kept.
        assert!(!s.contains('*'), "got: {s:?}");
        assert!(!s.contains('~'), "got: {s:?}");
        assert!(s.contains("important"));
        assert!(s.contains("critical"));
    }

    #[test]
    fn sanitize_handles_blockquote() {
        let s = sanitize_drawer_body("> quoted line\n> second line");
        assert!(!s.contains('>'), "marker leaked: {s:?}");
        assert!(s.contains("quoted line"));
    }

    #[test]
    fn sanitize_handles_hr_and_break() {
        let s = sanitize_drawer_body("alpha\n\n---\n\nbeta");
        // The hr line has no semantic content, so beta should still
        // appear after alpha. Underscore / hyphen can leak as part of
        // the surrounding text but the markdown thematic-break itself
        // shouldn't surface as searchable tokens.
        assert!(s.contains("alpha"));
        assert!(s.contains("beta"));
    }

    #[test]
    fn sanitize_falls_back_to_input_on_unparseable() {
        // markdown-rs is permissive, so triggering a real parse error
        // is rare. We construct a degenerate input (NUL byte) and
        // accept either a clean string or the raw passthrough — what
        // we don't accept is a panic.
        let s = sanitize_drawer_body("hello\0world");
        assert!(s.contains("hello") || s.contains("world"), "got: {s:?}");
    }

    #[test]
    fn collapse_blank_lines_caps_at_one_blank() {
        let s = collapse_blank_lines("a\n\n\n\nb\n\n\n\nc");
        // At most two newlines between any two non-blank tokens.
        assert_eq!(s, "a\n\nb\n\nc");
    }
}
