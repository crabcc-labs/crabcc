use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Const,
    Var,
    Type,
    Macro,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    pub parent: Option<String>,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub visibility: Option<String>,
}

/// Name-only symbol projection for consumers that never read `signature`
/// (fuzzy/prefix FTS). Omitting the `signature` column means the FSST-encoded
/// blob is never fetched or decompressed — see `Store::iter_symbol_names`.
#[derive(Debug, Clone)]
pub struct SymbolName {
    pub name: String,
    pub kind: SymbolKind,
    pub parent: Option<String>,
    pub file: String,
    pub line_start: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src_file: String,
    pub src_symbol: Option<String>,
    pub dst_name: String,
    pub kind: String,
    pub line: u32,
}

/// A location hit returned by `refs` / `callers` / pattern queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_kind_serializes_as_snake_case() {
        // The CLI + MCP both hand SymbolKind to clients as JSON. Snake_case
        // (set via serde rename_all) is the contract.
        let pairs = [
            (SymbolKind::Function, "\"function\""),
            (SymbolKind::Method, "\"method\""),
            (SymbolKind::Class, "\"class\""),
            (SymbolKind::Struct, "\"struct\""),
            (SymbolKind::Enum, "\"enum\""),
            (SymbolKind::Trait, "\"trait\""),
            (SymbolKind::Interface, "\"interface\""),
            (SymbolKind::Const, "\"const\""),
            (SymbolKind::Var, "\"var\""),
            (SymbolKind::Type, "\"type\""),
            (SymbolKind::Macro, "\"macro\""),
        ];
        for (kind, expect) in pairs {
            let s = serde_json::to_string(&kind).unwrap();
            assert_eq!(s, expect, "kind: {kind:?}");
        }
    }

    #[test]
    fn symbol_kind_round_trips_through_serde() {
        // Belt-and-braces: ensures the deserialize side accepts what the
        // serialize side produces. Catches typos in rename_all variants.
        for kind in [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Trait,
            SymbolKind::Interface,
            SymbolKind::Const,
            SymbolKind::Var,
            SymbolKind::Type,
            SymbolKind::Macro,
        ] {
            let s = serde_json::to_string(&kind).unwrap();
            let back: SymbolKind = serde_json::from_str(&s).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn symbol_skips_optional_none_fields() {
        // `signature`, `parent`, and `visibility` are Option<String>. When
        // None, the JSON should still be valid; we don't enforce omission, but
        // we DO need round-trip stability for the MCP wire shape.
        let s = Symbol {
            name: "x".into(),
            kind: SymbolKind::Function,
            signature: None,
            parent: None,
            file: "a.rs".into(),
            line_start: 1,
            line_end: 2,
            visibility: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "x");
        assert_eq!(back.kind, SymbolKind::Function);
        assert!(back.signature.is_none());
    }

    #[test]
    fn symbol_round_trips_with_macro_kind() {
        // The new Macro variant must survive serde — this is what gets stored
        // by store.rs and indexed by fts.rs.
        let s = Symbol {
            name: "info".into(),
            kind: SymbolKind::Macro,
            signature: Some("macro_rules! info".into()),
            parent: None,
            file: "log.rs".into(),
            line_start: 10,
            line_end: 20,
            visibility: Some("pub".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"macro\""), "json: {json}");
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, SymbolKind::Macro);
        assert_eq!(back.visibility.as_deref(), Some("pub"));
    }

    #[test]
    fn edge_round_trip() {
        let e = Edge {
            src_file: "a.rs".into(),
            src_symbol: Some("main".into()),
            dst_name: "helper".into(),
            kind: "call".into(),
            line: 42,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: Edge = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dst_name, "helper");
        assert_eq!(back.line, 42);
    }

    #[test]
    fn hit_round_trip() {
        let h = Hit {
            file: "a.rs".into(),
            line: 7,
            col: 3,
            snippet: "foo()".into(),
        };
        let json = serde_json::to_string(&h).unwrap();
        let back: Hit = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file, "a.rs");
        assert_eq!(back.line, 7);
        assert_eq!(back.col, 3);
    }

    #[test]
    fn unknown_kind_string_fails_deserialize() {
        // Snake_case enum: typos like "Function" or "macroo" must error,
        // not silently round-trip into a default variant.
        let r: Result<SymbolKind, _> = serde_json::from_str("\"Function\"");
        assert!(r.is_err());
        let r: Result<SymbolKind, _> = serde_json::from_str("\"macroo\"");
        assert!(r.is_err());
    }
}
