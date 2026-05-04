//! Top-level route content views. One module per route — the shell
//! (`crate::shell`) renders the header + nav and switches the body
//! based on `AppState::route`. Each route view is self-contained and
//! observes `AppState` only for the slices it cares about.

pub mod agent_spawn_sheet;
pub mod agents;
pub mod commands;
pub mod dashboard;
pub mod empty;
pub mod graph;
pub mod k_graph;
pub mod knowledge;
pub mod logs;
pub mod system;
pub mod timeline;
