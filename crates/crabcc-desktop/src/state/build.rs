//! `build` — wires up [`super::AppState`] with background workers and
//! the optional MCP server, then returns it to the gpui context.

use std::sync::{Arc, RwLock};

use gpui::Context;
use tracing::info;

use super::workers::spawn_workers;
use super::{AppEvent, AppState, WorkerHandles};

/// Returns the AppState entity wired up with workers. Call from inside
/// a gpui context (e.g. `cx.new(|cx| build(cx, base))`).
pub fn build(cx: &mut Context<AppState>, base_url: &str) -> AppState {
    let (tx, rx) = spawn_workers(base_url);
    let entity = cx.entity();
    AppState::pump_events(&entity, rx, cx);
    // Pre-allocate the resource snapshot Arc — shared between the
    // sampling-handler's ResourceProvider (worker thread) and the
    // AppState writer (gpui thread, via `apply()` → refresh).
    // Always created; only stashed on AppState if the MCP server
    // actually starts (saves the per-event refresh cost otherwise).
    let resource_snapshot = Arc::new(RwLock::new(crate::resources::ResourceSnapshot::default()));
    let mcp_server = try_start_mcp_server(tx.clone(), resource_snapshot.clone());
    let resource_snapshot_field = mcp_server.as_ref().map(|_| resource_snapshot);
    let mut state = AppState {
        workers: Some(WorkerHandles {
            tx,
            base_url: base_url.to_string(),
        }),
        mcp_server,
        resource_snapshot: resource_snapshot_field,
        ..AppState::new()
    };
    // Inspector-route demo seed. Opt-in via env var so production
    // binaries don't carry synthetic state. See
    // `crate::inspector::seed_demo_events` for the event set.
    if std::env::var_os("CRABCC_DESKTOP_INSPECTOR_DEMO").is_some() {
        crate::inspector::seed_demo_events(&mut state);
    }
    state
}

/// Best-effort MCP server startup. Resolves the socket path,
/// builds a [`crate::sampling::LiteLlmSamplingHandler`] from env,
/// wires the inspector observer + the resource provider, spawns
/// the listener thread. Returns `None` (with a warn log) on any
/// failure — the desktop continues running without an MCP surface,
/// which is the right behaviour for hosts that haven't set
/// `LITELLM_MASTER_KEY`.
fn try_start_mcp_server(
    tx: flume::Sender<AppEvent>,
    resource_snapshot: Arc<RwLock<crate::resources::ResourceSnapshot>>,
) -> Option<crate::mcp_server::McpServerHandle> {
    let socket_path = match crate::mcp_server::default_socket_path() {
        Some(p) => p,
        None => {
            info!(
                target: "crabcc::state",
                "MCP server: no socket path resolvable (HOME unset?); skipping",
            );
            return None;
        }
    };

    let handler = match crate::sampling::LiteLlmSamplingHandler::from_env() {
        Ok(h) => h,
        Err(e) => {
            info!(
                target: "crabcc::state",
                error = %e,
                "MCP server: sampling handler not available; skipping",
            );
            return None;
        }
    };
    let observer = Arc::new(crate::inspector::InspectorSamplingObserver::new(tx));
    let provider = Arc::new(crate::resources::AppStateResourceProvider::new(
        resource_snapshot,
    ));
    let handler = Arc::new(
        handler
            .with_observer(observer)
            .with_resource_provider(provider),
    );
    let tools = Arc::new(crate::tools::ToolRegistry::with_defaults());

    match crate::mcp_server::spawn(socket_path.clone(), handler, tools) {
        Ok(h) => {
            info!(
                target: "crabcc::state",
                path = %socket_path.display(),
                "MCP server started",
            );
            Some(h)
        }
        Err(e) => {
            tracing::warn!(
                target: "crabcc::state",
                error = %e,
                path = %socket_path.display(),
                "MCP server startup failed",
            );
            None
        }
    }
}
