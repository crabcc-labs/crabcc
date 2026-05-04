//! Theme palette mirroring the `crabcc-viz/web` dashboard.
//!
//! The desktop crate uses gpui-component's default theme out of the
//! box. The web dashboard at `/live` ships a custom light/dark
//! palette pinned at the top of `crates/crabcc-viz/web/src/styles.css`
//! plus a cyberpunk-skin overlay on a few routes. To make the two
//! surfaces visually consistent, this module overrides the relevant
//! `ThemeColor` fields after `gpui_component::init` has run.
//!
//! Strategy:
//!
//! - Match the web's dark-mode core palette token-for-token. The web
//!   ships both light and dark; the desktop binary follows the OS
//!   appearance (gpui-component's `Theme::sync_system_appearance`),
//!   so we override BOTH branches and let the runtime pick.
//! - Only touch the tokens that diverge from gpui-component's
//!   defaults — leave `info`, `warning`, `accent`, `secondary`-fg
//!   etc. alone unless the web specifies them.
//! - The cyberpunk-panel accents (cyan #6df0ff, hot-pink #ff2a6d,
//!   amber #ff8c2a, agent-text #c8d4ff) are panel-specific in the
//!   web; they're surfaced here as `pub const`s so per-route widgets
//!   can opt into them without leaking them into the global theme.

use gpui::{rgb, App, Hsla};
use gpui_component::theme::Theme;

/// Cyberpunk accent — main cyan glow used by the Ollama key panel,
/// services panel "ok" state, agent code blocks, and various
/// interactive accent treatments on the web.
pub const CYBER_CYAN: u32 = 0x6df0ff;
/// Cyberpunk accent — hot-pink button border + filled hover state
/// used by the Ollama key panel reveal/copy buttons on the web.
pub const CYBER_PINK: u32 = 0xff2a6d;
/// Cyberpunk accent — amber warning used for `key-mode.warn`,
/// `service-state.down`, the "down" path on services-panel rows.
pub const CYBER_AMBER: u32 = 0xff8c2a;
/// Cyberpunk accent — agent text body colour (`#c8d4ff`) used as
/// the foreground inside the agent panels and table headers.
pub const AGENT_TEXT: u32 = 0xc8d4ff;
/// Cyberpunk accent — muted blue body colour (`#8aa0d8`) used for
/// secondary metadata rows on the agent / services panels.
pub const AGENT_MUTED: u32 = 0x8aa0d8;
/// Background of the cyberpunk key-row gradient — left + right
/// stops both use this near-black value.
pub const CYBER_BG_DEEP: u32 = 0x0a0f1e;

/// Apply the web-mirroring palette to gpui-component's global
/// theme. Call from `main` after `gpui_component::init(cx)`.
///
/// Overrides BOTH the dark and light paths (the runtime's
/// `sync_system_appearance` flips between them); each token is
/// pinned to the corresponding `--var` in `styles.css`.
pub fn install(cx: &mut App) {
    let theme = Theme::global_mut(cx);

    // The dark-mode CSS vars from styles.css :root + @media. Picked
    // because the OS dark-mode is the typical crabcc operator
    // setup (terminal-adjacent surface, low ambient light).
    if theme.is_dark() {
        theme.background = rgb(0x0e0e10).into(); // --bg
        theme.foreground = rgb(0xe8e8e8).into(); // --fg
        theme.muted_foreground = rgb(0x8a8a8a).into(); // --muted
                                                       // Web's `--panel` maps to gpui-component's `secondary`
                                                       // — that's the elevated-panel background used by the
                                                       // existing tile / card surfaces.
        theme.secondary = rgb(0x161618).into();
        theme.border = rgb(0x2a2a2c).into();
        theme.primary = rgb(0xff8c42).into(); // --accent
        theme.success = rgb(0x2ecc71).into(); // --live-ok
        theme.danger = rgb(0xff5757).into(); // brighter than the
                                             // web's destructive
                                             // since the dashboard
                                             // is Notification-Center-
                                             // adjacent.
    } else {
        theme.background = rgb(0xfafafa).into();
        theme.foreground = rgb(0x1a1a1a).into();
        theme.muted_foreground = rgb(0x6a6a6a).into();
        theme.secondary = rgb(0xffffff).into();
        theme.border = rgb(0xe3e3e3).into();
        theme.primary = rgb(0xd35400).into();
        theme.success = rgb(0x27ae60).into();
        theme.danger = rgb(0xc0392b).into();
    }
}

/// Convenience wrapper — converts one of the cyberpunk-accent
/// `u32` consts to an `Hsla`. Lets per-route code write
/// `theme::cyber(theme::CYBER_CYAN)` instead of the slightly
/// noisier `gpui::rgb(theme::CYBER_CYAN).into()`.
#[inline]
pub fn cyber(hex: u32) -> Hsla {
    rgb(hex).into()
}
