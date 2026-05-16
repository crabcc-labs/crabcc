//! Pure (no I/O orchestration) handlers that convert crabcc-core query
//! results into LSP wire types. Kept separate from `server.rs` so the
//! mapping is unit-testable.

use crabcc_core::types::{Hit, Symbol, SymbolKind as CcKind};
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, DocumentSymbol, Hover,
    HoverContents, Location, MarkupContent, MarkupKind, Position, Range, SymbolInformation,
    SymbolKind as LspKind, Url, WorkspaceSymbol,
};

pub fn to_url(root: &Path, rel: &str) -> Option<Url> {
    let abs: PathBuf = root.join(rel);
    Url::from_file_path(abs).ok()
}

/// Convert an LSP URL into the repo-relative path the Store uses.
pub fn rel_from_url(root: &Path, url: &Url) -> Option<String> {
    let abs = url.to_file_path().ok()?;
    let rel = abs.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

pub fn lsp_kind(k: CcKind) -> LspKind {
    match k {
        CcKind::Function => LspKind::FUNCTION,
        CcKind::Method => LspKind::METHOD,
        CcKind::Class => LspKind::CLASS,
        CcKind::Struct => LspKind::STRUCT,
        CcKind::Enum => LspKind::ENUM,
        CcKind::Trait => LspKind::INTERFACE,
        CcKind::Interface => LspKind::INTERFACE,
        CcKind::Const => LspKind::CONSTANT,
        CcKind::Var => LspKind::VARIABLE,
        CcKind::Type => LspKind::TYPE_PARAMETER,
        CcKind::Macro => LspKind::FUNCTION,
    }
}

fn symbol_range(s: &Symbol) -> Range {
    Range {
        start: Position {
            line: s.line_start.saturating_sub(1),
            character: 0,
        },
        end: Position {
            line: s.line_end.saturating_sub(1),
            character: 0,
        },
    }
}

fn hit_range(h: &Hit) -> Range {
    let line = h.line.saturating_sub(1);
    let col = h.col.saturating_sub(1);
    Range {
        start: Position {
            line,
            character: col,
        },
        end: Position {
            line,
            character: col + 1,
        },
    }
}

pub fn document_symbols(symbols: Vec<Symbol>) -> Vec<DocumentSymbol> {
    symbols
        .into_iter()
        .map(|s| {
            let range = symbol_range(&s);
            #[allow(deprecated)]
            DocumentSymbol {
                name: s.name.clone(),
                detail: s.signature.clone(),
                kind: lsp_kind(s.kind),
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            }
        })
        .collect()
}

pub fn definition_locations(root: &Path, symbols: Vec<Symbol>) -> Vec<Location> {
    symbols
        .into_iter()
        .filter_map(|s| {
            let uri = to_url(root, &s.file)?;
            Some(Location {
                uri,
                range: symbol_range(&s),
            })
        })
        .collect()
}

pub fn reference_locations(root: &Path, hits: Vec<Hit>) -> Vec<Location> {
    hits.into_iter()
        .filter_map(|h| {
            let uri = to_url(root, &h.file)?;
            Some(Location {
                uri,
                range: hit_range(&h),
            })
        })
        .collect()
}

pub fn hover_for(symbols: &[Symbol]) -> Option<Hover> {
    let s = symbols.first()?;
    let mut md = String::with_capacity(160);
    md.push_str("```\n");
    if let Some(sig) = &s.signature {
        md.push_str(sig);
    } else {
        md.push_str(&s.name);
    }
    md.push_str("\n```\n\n");
    md.push_str(&format!(
        "**{:?}** in `{}:{}`",
        s.kind, s.file, s.line_start
    ));
    if let Some(parent) = &s.parent {
        md.push_str(&format!("\n\nParent: `{}`", parent));
    }
    if symbols.len() > 1 {
        md.push_str(&format!(
            "\n\n_{} definitions match this name; showing the first._",
            symbols.len()
        ));
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    })
}

#[allow(deprecated)]
pub fn workspace_symbol_legacy(root: &Path, symbols: Vec<Symbol>) -> Vec<SymbolInformation> {
    symbols
        .into_iter()
        .filter_map(|s| {
            let uri = to_url(root, &s.file)?;
            Some(SymbolInformation {
                name: s.name.clone(),
                kind: lsp_kind(s.kind),
                tags: None,
                deprecated: None,
                location: Location {
                    uri,
                    range: symbol_range(&s),
                },
                container_name: s.parent.clone(),
            })
        })
        .collect()
}

pub fn call_hierarchy_item(root: &Path, s: &Symbol) -> Option<CallHierarchyItem> {
    let uri = to_url(root, &s.file)?;
    Some(CallHierarchyItem {
        name: s.name.clone(),
        kind: lsp_kind(s.kind),
        tags: None,
        detail: s.signature.clone(),
        uri,
        range: symbol_range(s),
        selection_range: symbol_range(s),
        data: None,
    })
}

pub fn incoming_calls(
    root: &Path,
    caller_name: &str,
    hits: Vec<Hit>,
) -> Vec<CallHierarchyIncomingCall> {
    hits.into_iter()
        .filter_map(|h| {
            let uri = to_url(root, &h.file)?;
            let item = CallHierarchyItem {
                name: caller_name.to_string(),
                kind: LspKind::FUNCTION,
                tags: None,
                detail: Some(h.snippet),
                uri,
                range: hit_range(&Hit {
                    file: h.file.clone(),
                    line: h.line,
                    col: h.col,
                    snippet: String::new(),
                }),
                selection_range: hit_range(&Hit {
                    file: h.file.clone(),
                    line: h.line,
                    col: h.col,
                    snippet: String::new(),
                }),
                data: None,
            };
            Some(CallHierarchyIncomingCall {
                from: item,
                from_ranges: Vec::new(),
            })
        })
        .collect()
}

pub fn outgoing_calls(root: &Path, callees: Vec<Symbol>) -> Vec<CallHierarchyOutgoingCall> {
    callees
        .into_iter()
        .filter_map(|s| {
            let item = call_hierarchy_item(root, &s)?;
            Some(CallHierarchyOutgoingCall {
                to: item,
                from_ranges: Vec::new(),
            })
        })
        .collect()
}

#[allow(dead_code)]
pub fn workspace_symbol_new(root: &Path, symbols: Vec<Symbol>) -> Vec<WorkspaceSymbol> {
    symbols
        .into_iter()
        .filter_map(|s| {
            let uri = to_url(root, &s.file)?;
            Some(WorkspaceSymbol {
                name: s.name.clone(),
                kind: lsp_kind(s.kind),
                tags: None,
                container_name: s.parent.clone(),
                location: tower_lsp::lsp_types::OneOf::Left(Location {
                    uri,
                    range: symbol_range(&s),
                }),
                data: None,
            })
        })
        .collect()
}
