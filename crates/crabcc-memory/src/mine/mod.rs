//! Miners — bulk drawer producers.
//!
//! Two flavours, both idempotent (re-running emits zero new drawers):
//!
//! - [`project`] — walks a repository via `crabcc_core::walker::walk_repo`,
//!   one drawer per text file at `wing="proj"` / `source_id="proj:<rel>"`.
//! - [`sessions`] — parses Claude Code JSONL transcripts under
//!   `~/.claude/projects/<repo>/`, one drawer per `(user, assistant)` turn
//!   pair at `wing="session"` / `source_id="session:<file>:<pair>"`.
//!
//! Idempotency falls out of the existing `(source_id, sha256)` UNIQUE
//! constraint enforced by [`crate::backend::Backend::add`] — the miner
//! itself stays stateless.

pub mod project;
pub mod sessions;

use serde::{Deserialize, Serialize};

/// Summary of one mining run. Returned by [`project::mine_project`] and
/// [`sessions::mine_sessions`]; surfaced as the JSON body of
/// `crabcc memory mine` and the MCP `memory.mine_*` tools.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MineReport {
    /// Number of input units examined (files for `project`, JSONL lines
    /// for `sessions`). Reported pre-dedup so the user can see whether a
    /// re-run actually scanned anything.
    pub scanned: usize,
    /// Drawers handed to the backend. Equal to scanned minus skipped
    /// units (binary / oversize / unparseable).
    pub considered: usize,
    /// Drawers that landed as fresh rows (i.e. the `(source_id, sha256)`
    /// pair was new). On a repeat run this drops to zero.
    pub inserted: usize,
    /// Drawers that already existed (idempotent hit).
    pub deduped: usize,
    /// Inputs the miner refused — per kind, see [`SkipReason`].
    pub skipped: usize,
}

impl MineReport {
    pub fn record_inserted(&mut self) {
        self.considered += 1;
        self.inserted += 1;
    }

    pub fn record_dedup(&mut self) {
        self.considered += 1;
        self.deduped += 1;
    }

    pub fn record_skip(&mut self) {
        self.skipped += 1;
    }
}

/// Why a candidate was not turned into a drawer. Surfaced via tracing so
/// users can spot a directory full of `Binary` skips and adjust globs.
#[derive(Debug, Clone, Copy)]
pub enum SkipReason {
    /// First chunk contained a NUL byte → assumed binary.
    Binary,
    /// File or turn-pair body exceeded the configured cap.
    OverSize,
    /// JSONL line failed to parse, or had no extractable text content.
    Unparseable,
    /// Empty body after sanitization — would not contribute to recall.
    Empty,
}
