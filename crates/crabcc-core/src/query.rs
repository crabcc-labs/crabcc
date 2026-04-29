use crate::store::Store;
use crate::types::Symbol;
use anyhow::Result;

pub fn find_symbol(store: &Store, name: &str) -> Result<Vec<Symbol>> {
    store.find_by_name(name)
}

// TODO: refs(name) — pull from `edges` table. Falls back to ripgrep
//       textual search when graph edges are missing.
pub fn refs(_store: &Store, _name: &str) -> Result<Vec<()>> {
    Ok(Vec::new())
}

// TODO: callers(name) — edges where dst_name = name AND kind = 'call'.
pub fn callers(_store: &Store, _name: &str) -> Result<Vec<()>> {
    Ok(Vec::new())
}
