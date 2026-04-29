// Tantivy-backed full-text sidecar for fuzzy + prefix search over symbol names.
//
// Lives at .crabcc/tantivy/. Built by `crabcc fts-rebuild` (cheap — a few
// seconds for ~38k symbols on mc-mothership). `crabcc index` rebuilds it
// automatically; `crabcc refresh` does NOT (Tantivy stays as-of-last-index
// until rebuilt). Documented in the skill so the agent doesn't get
// confused by stale fuzzy hits.

use crate::store::Store;
use crate::types::SymbolKind;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{FuzzyTermQuery, RegexQuery};
use tantivy::schema::{Field, Schema, STORED, STRING, TEXT};
use tantivy::{doc, Index, ReloadPolicy, TantivyDocument, Term};

pub struct Fts {
    index: Index,
    f_name: Field,
    f_kind: Field,
    f_file: Field,
    f_line: Field,
    f_parent: Field,
}

#[derive(Debug, Clone, Serialize)]
pub struct FuzzyHit {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u64,
    pub parent: Option<String>,
    pub score: f32,
}

impl Fts {
    pub fn open(dir: &Path) -> Result<Self> {
        let mut sb = Schema::builder();
        let f_name = sb.add_text_field("name", TEXT | STORED);
        let f_kind = sb.add_text_field("kind", STRING | STORED);
        let f_file = sb.add_text_field("file", STRING | STORED);
        let f_line = sb.add_u64_field("line", STORED);
        let f_parent = sb.add_text_field("parent", STRING | STORED);
        let schema = sb.build();
        std::fs::create_dir_all(dir)?;
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema)?;
        Ok(Self {
            index,
            f_name,
            f_kind,
            f_file,
            f_line,
            f_parent,
        })
    }

    /// Drop everything and reindex from the current SQLite store.
    pub fn rebuild(&self, store: &Store) -> Result<usize> {
        let mut writer = self.index.writer(50_000_000)?;
        writer.delete_all_documents()?;
        let symbols = store.iter_all_symbols()?;
        let n = symbols.len();
        for s in symbols {
            writer.add_document(doc!(
                self.f_name   => s.name,
                self.f_kind   => kind_str(s.kind),
                self.f_file   => s.file,
                self.f_line   => s.line_start as u64,
                self.f_parent => s.parent.unwrap_or_default(),
            ))?;
        }
        writer.commit()?;
        Ok(n)
    }

    pub fn fuzzy(&self, query: &str, limit: usize) -> Result<Vec<FuzzyHit>> {
        let term = Term::from_field_text(self.f_name, &query.to_lowercase());
        let q = FuzzyTermQuery::new(term, 2, true);
        self.exec(&q, limit)
    }

    pub fn prefix(&self, query: &str, limit: usize) -> Result<Vec<FuzzyHit>> {
        let pat = format!("{}.*", regex_escape(&query.to_lowercase()));
        let q = RegexQuery::from_pattern(&pat, self.f_name)?;
        self.exec(&q, limit)
    }

    fn exec(&self, q: &dyn tantivy::query::Query, limit: usize) -> Result<Vec<FuzzyHit>> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        let top = searcher.search(q, &TopDocs::with_limit(limit))?;
        let mut hits = Vec::new();
        for (score, addr) in top {
            let d: TantivyDocument = searcher.doc(addr)?;
            hits.push(self.doc_to_hit(&d, score));
        }
        Ok(hits)
    }

    fn doc_to_hit(&self, d: &TantivyDocument, score: f32) -> FuzzyHit {
        use tantivy::schema::OwnedValue;
        fn s(v: Option<&OwnedValue>) -> String {
            match v {
                Some(OwnedValue::Str(x)) => x.clone(),
                _ => String::new(),
            }
        }
        fn u(v: Option<&OwnedValue>) -> u64 {
            match v {
                Some(OwnedValue::U64(x)) => *x,
                _ => 0,
            }
        }
        let parent = s(d.get_first(self.f_parent));
        FuzzyHit {
            name: s(d.get_first(self.f_name)),
            kind: s(d.get_first(self.f_kind)),
            file: s(d.get_first(self.f_file)),
            line: u(d.get_first(self.f_line)),
            parent: if parent.is_empty() {
                None
            } else {
                Some(parent)
            },
            score,
        }
    }
}

fn regex_escape(s: &str) -> String {
    const SPECIALS: &str = r".+*?^$|[](){}\";
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if SPECIALS.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Const => "const",
        SymbolKind::Var => "var",
        SymbolKind::Type => "type",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build_index;

    fn fixture() -> (tempfile::TempDir, Store, Fts) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("a.ts"),
            "export function getUserProfile(){};\n\
             export function getUserAvatar(){};\n\
             export class UserSession {};\n\
             export type Settings = {};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("b.rb"),
            "class Authenticator\n  def authenticate; end\nend\n",
        )
        .unwrap();
        let store = Store::open(&root.join("idx.db")).unwrap();
        build_index(root, &store).unwrap();
        let fts_dir = root.join("tantivy");
        let fts = Fts::open(&fts_dir).unwrap();
        let n = fts.rebuild(&store).unwrap();
        assert!(n >= 5, "expected ≥5 symbols, got {n}");
        (dir, store, fts)
    }

    #[test]
    fn prefix_finds_user_symbols() {
        let (_dir, _store, fts) = fixture();
        let hits = fts.prefix("getUser", 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.starts_with("getUser")),
            "got: {names:?}"
        );
    }

    #[test]
    fn fuzzy_tolerates_typo() {
        // "Authentcator" missing an 'i' — Levenshtein distance 1.
        let (_dir, _store, fts) = fixture();
        let hits = fts.fuzzy("Authentcator", 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.contains("Authenticator")),
            "fuzzy should match Authenticator, got: {names:?}"
        );
    }

    #[test]
    fn fuzzy_returns_score() {
        let (_dir, _store, fts) = fixture();
        let hits = fts.fuzzy("UserSession", 5).unwrap();
        assert!(hits.iter().any(|h| h.score > 0.0));
    }

    #[test]
    fn rebuild_is_idempotent() {
        let (_dir, store, fts) = fixture();
        let n1 = fts.rebuild(&store).unwrap();
        let n2 = fts.rebuild(&store).unwrap();
        assert_eq!(n1, n2);
    }
}
