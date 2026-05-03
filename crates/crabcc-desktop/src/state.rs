use gpui::SharedString;

/// Information loaded once at startup from the bootstrapped environment.
#[derive(Debug, Clone)]
pub struct BootstrapInfo {
    pub version: SharedString,
    pub repo: SharedString,
}

/// Top-level navigation route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    #[default]
    Home,
    Settings,
    Agents,
}

/// A single tracked agent run.
#[derive(Debug)]
pub struct AgentRun {
    pub id: u64,
    pub running: bool,
}

/// Top-level application state held in a GPUI `Entity<AppState>`.
#[derive(Debug)]
pub struct AppState {
    pub bootstrap: Option<BootstrapInfo>,
    pub route: Route,
    /// Tracked agent runs; populated by an external controller.
    agents: Vec<AgentRun>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            bootstrap: None,
            route: Route::default(),
            agents: Vec::new(),
        }
    }

    /// Count of currently-running agent processes.
    pub fn agents_running(&self) -> u32 {
        self.agents.iter().filter(|a| a.running).count() as u32
    }
}
