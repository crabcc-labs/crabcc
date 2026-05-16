use std::path::Path;

/// Languages ucracc-lsp explicitly supports. Languages parsed by this
/// crate (Swift, Bash, YAML, Markdown) are gated on the respective
/// Cargo features; everything else is delegated to crabcc-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Ruby,
    Go,
    Swift,
    Bash,
    Java,
    Yaml,
    Markdown,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
            Lang::Ruby => "ruby",
            Lang::Go => "go",
            Lang::Swift => "swift",
            Lang::Bash => "bash",
            Lang::Java => "java",
            Lang::Yaml => "yaml",
            Lang::Markdown => "markdown",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        Some(match ext {
            "rs" => Lang::Rust,
            "ts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "py" | "pyi" => Lang::Python,
            "rb" | "rake" | "gemspec" => Lang::Ruby,
            "go" => Lang::Go,
            "swift" => Lang::Swift,
            "sh" | "bash" | "zsh" => Lang::Bash,
            "java" => Lang::Java,
            "yaml" | "yml" => Lang::Yaml,
            "md" | "markdown" => Lang::Markdown,
            _ => return None,
        })
    }

    pub fn from_path(p: &Path) -> Option<Self> {
        p.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_ext)
    }

    /// Languages parsed by this crate (vs delegated to crabcc-core).
    /// Swift and Bash moved into crabcc-core in v0.2.0; only data-shaped
    /// languages (YAML, Markdown) stay here because they're not code and
    /// don't fit crabcc-core's symbol-extractor mission.
    pub fn handled_internally(self) -> bool {
        matches!(self, Lang::Yaml | Lang::Markdown)
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
    "ruby",
    "go",
    "swift",
    "shellscript",
    "java",
    "yaml",
    "markdown",
];
