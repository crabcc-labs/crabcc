//! Top-level route content views. One module per route — the shell
//! (`crate::shell`) renders the header + nav and switches the body
//! based on `AppState::route`. Each route view is self-contained and
//! observes `AppState` only for the slices it cares about.

pub mod dashboard;
pub mod graph;
pub mod knowledge;
pub mod logs;
pub mod system;
