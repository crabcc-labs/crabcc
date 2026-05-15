use std::path::Path;

/// Languages ucracc-lsp explicitly supports. Anything outside this set
/// falls through to crabcc-core's own detection (which adds Ruby + Go),
/// or is skipped if even that doesn't recognise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Swift,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
            Lang::Swift => "swift",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        Some(match ext {
            "rs" => Lang::Rust,
            "ts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "py" | "pyi" => Lang::Python,
            "swift" => Lang::Swift,
            _ => return None,
        })
    }

    pub fn from_path(p: &Path) -> Option<Self> {
        p.extension().and_then(|e| e.to_str()).and_then(Self::from_ext)
    }

    /// Is this a language ucracc-lsp handles internally (Swift), as opposed
    /// to one crabcc-core indexes for us?
    pub fn handled_internally(self) -> bool {
        matches!(self, Lang::Swift)
    }
}

/// The set of LSP `documentSelector` languageIds we advertise in
/// `initialize`'s ServerCapabilities. Order matters for some clients.
pub const SUPPORTED_LANGUAGE_IDS: &[&str] = &[
    "rust",
    "typescript",
    "typescriptreact",
    "javascript",
    "javascriptreact",
    "python",
    "swift",
];
