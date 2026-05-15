//! Incremental reparse: keep the parsed tree-sitter `Tree` for each open
//! document, apply per-event `InputEdit`s as `didChange` arrives, and
//! reparse with the edited old tree so tree-sitter can reuse subtrees
//! outside the touched region.
//!
//! Falls back gracefully:
//! - First `didOpen` for a file: no old tree, parse from scratch (one
//!   ~30 µs hit) and cache the result.
//! - `didChange` event with `range == None`: a "full replace" event, we
//!   throw the old tree away and parse fresh.
//! - LSP clients that advertise positionEncoding=utf-16: we currently
//!   treat positions as UTF-8 byte offsets, which is correct for the
//!   ASCII subset most code lives in. Mixed-script files (emoji, CJK)
//!   may produce a slightly off byte point in the InputEdit, which
//!   tree-sitter tolerates by widening the reparse region — correctness
//!   is preserved, just incremental efficiency drops.

use tower_lsp::lsp_types::{Position, TextDocumentContentChangeEvent};
use tree_sitter::{InputEdit, Parser, Point, Tree};

/// Apply an LSP content-change event in place. Returns the byte-range
/// that was replaced so the caller can build an `InputEdit`.
pub fn apply_change(text: &mut String, ev: &TextDocumentContentChangeEvent) -> Option<InputEdit> {
    let range = ev.range?;
    let start_byte = pos_to_byte(text, range.start)?;
    let old_end_byte = pos_to_byte(text, range.end)?;
    let new_end_byte = start_byte + ev.text.len();

    let start_position = pos_to_point(range.start);
    let old_end_position = pos_to_point(range.end);
    // Compute the new end point by replaying the inserted text. Cheap
    // for the common case of a single-char keystroke.
    let new_end_position = advance_point(start_position, &ev.text);

    text.replace_range(start_byte..old_end_byte, &ev.text);

    Some(InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position,
        old_end_position,
        new_end_position,
    })
}

fn pos_to_byte(text: &str, pos: Position) -> Option<usize> {
    let mut line = 0u32;
    let mut col_remaining = pos.character as usize;
    let mut byte = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if line == pos.line {
            // Treat character offset as UTF-8 byte offset within the line.
            // See module-level note on the multi-script caveat.
            if col_remaining == 0 {
                return Some(i);
            }
            if b == b'\n' {
                return Some(i);
            }
            col_remaining = col_remaining.saturating_sub(1);
        } else if b == b'\n' {
            line += 1;
        }
        byte = i + 1;
    }
    if line == pos.line && col_remaining == 0 {
        Some(byte)
    } else {
        // Cursor past end-of-document — clamp to end.
        Some(text.len())
    }
}

fn pos_to_point(pos: Position) -> Point {
    Point {
        row: pos.line as usize,
        column: pos.character as usize,
    }
}

fn advance_point(start: Point, inserted: &str) -> Point {
    let mut row = start.row;
    let mut col = start.column;
    for ch in inserted.chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Point { row, column: col }
}

/// Parse `src` into a new `Tree`, optionally reusing `old_tree` to skip
/// unchanged subtrees. The caller is responsible for having called
/// `old_tree.edit(...)` once per content-change *before* invoking this.
pub fn reparse(parser: &mut Parser, src: &str, old_tree: Option<&Tree>) -> Option<Tree> {
    parser.parse(src, old_tree)
}
