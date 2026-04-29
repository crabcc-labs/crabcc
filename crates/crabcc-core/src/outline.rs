// File outline: top-level symbols only, signatures, no bodies.
// Designed for compact LLM consumption.

use crate::store::Store;
use crate::types::Symbol;
use anyhow::Result;

pub fn outline(_store: &Store, _file: &str) -> Result<Vec<Symbol>> {
    Ok(Vec::new())
}
