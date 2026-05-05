//! Crabcc's supervisor + telemetry layer.
//!
//! ## Architecture
//!
//! ```text
//!   ~/.crabcc/_internal.db
//!   ├── (existing tables: agent_runs, agent_kill_events, backup_runs)
//!   └── (new) _crab_metadata, _crab_host, _crab_session, _crab_event,
//!             _crab_resource_sample, _crab_crash
//!
//!   crabcc-godfather (this crate)
//!   ├── lib       — Godfather API embedded by other crates
//!   └── bin       — standalone `crabcc-godfather watch | kill | …`
//! ```
//!
//! Other crates open the same DB and call `Godfather::record_*`
//! directly. The standalone binary supervises a child process via
//! [`WatchHandle`] without sharing memory — useful for the
//! "supervise the dashboard from outside the dashboard" case where
//! a viz crash must NOT also kill the supervisor.
//!
//! ## Privacy contract
//!
//! No PII ever lands in `_crab_*` rows. Specifically:
//!
//!   * Hostname and machine UUID are SHA-256 hashed (16-hex-char
//!     prefix) before insertion. Raw values never reach the DB.
//!   * No usernames / paths / IPs / MACs are recorded.
//!   * `CRABCC_NO_TELEMETRY=1` disables every write at the
//!     `Godfather::open` layer — every `record_*` becomes a no-op.
//!     Enforced once at construction, not per-call, so the CPU cost
//!     of opt-out is one env-var check.

pub mod cleanup;
pub mod control;
pub mod event;
pub mod godfather;
pub mod host;
pub mod report;
pub mod schema;
pub mod session;
pub mod watch;

pub use cleanup::{prune_if_due, prune_now, PruneStats, Retention};

pub use event::{Event, Severity};
pub use godfather::{Godfather, InstallSource};
pub use host::HostInfo;
pub use session::{Session, SessionId};
pub use watch::{WatchConfig, WatchHandle};
