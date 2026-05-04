//! Theme palette mirroring the `crabcc-viz/web` dashboard.
//!
//! Strategy:
//!
//! - Every palette ships as a single `Palette` const. No
//!   `if dark { ... } else { ... }` branching inside `install` —
//!   the runtime picks one of two presets based on OS appearance,
//!   or honours an explicit `CRABCC_DESKTOP_PALETTE` env override.
//! - Adding a new palette = one `pub const NAME: Palette = Self {
//!   ... };`. The render path doesn't change.
//! - The cyberpunk-panel accents (cyan / hot-pink / amber /
//!   agent-text / agent-muted / deep-bg) live ON the palette as
//!   first-class tokens so per-route widgets can read them
//!   uniformly via `cx.theme().cyber_cyan()` (see helpers below).
//!
//! Override at runtime:
//!
//! ```sh
//! CRABCC_DESKTOP_PALETTE=cyberpunk_neon cargo run --release
//! ```
//!
//! Available palette names: `web_dark`, `web_light`,
//! `cyberpunk_neon`, `mono`, `high_contrast`. Unknown values fall
//! back to the OS-appearance pair. Full list lives at
//! [`Palette::ALL_NAMES`].

use gpui::{rgb, App, Hsla};
use gpui_component::theme::Theme;

/// All tokens needed to skin both the gpui-component core
/// (`background` / `foreground` / etc.) and the cyberpunk panel
/// accents in one shot. Stored as `u32` hex so palette
/// definitions stay declarative — `install` converts to `Hsla`
/// at apply time.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window background — gpui-component's `background`.
    /// Mirrors `--bg` in styles.css.
    pub background: u32,
    /// Default text colour — gpui-component's `foreground`.
    /// Mirrors `--fg`.
    pub foreground: u32,
    /// Secondary text colour — gpui-component's
    /// `muted_foreground`. Mirrors `--muted`.
    pub muted_foreground: u32,
    /// Elevated panel surface — gpui-component's `secondary`.
    /// Mirrors `--panel`.
    pub secondary: u32,
    /// Border / divider colour. Mirrors `--border`.
    pub border: u32,
    /// Brand / accent colour. Mirrors `--accent`.
    pub primary: u32,
    /// Success / live state. Mirrors `--live-ok`.
    pub success: u32,
    /// Destructive / down state. Brighter than the web's
    /// destructive since the dashboard is Notification-Center-
    /// adjacent and competes with system-banner red.
    pub danger: u32,

    // ── Cyberpunk panel accents — opt-in, per-route ─────────
    /// Cyan glow used by the Ollama key reveal/copy row, the
    /// services-state "ok" rows, and code blocks on the agent
    /// panels (`#6df0ff` in the web).
    pub cyber_cyan: u32,
    /// Hot-pink button accent on the Ollama key reveal/copy
    /// buttons (`#ff2a6d`).
    pub cyber_pink: u32,
    /// Amber warning — `key-mode.warn`, `service-state.down`,
    /// and the down path on the services panel (`#ff8c2a`).
    pub cyber_amber: u32,
    /// Agent-panel body text (`#c8d4ff`).
    pub agent_text: u32,
    /// Agent-panel muted metadata (`#8aa0d8`).
    pub agent_muted: u32,
    /// Deep BG colour for the cyberpunk gradient stops
    /// (`#0a0f1e`).
    pub cyber_bg_deep: u32,
}

impl Palette {
    /// Mirrors `crates/crabcc-viz/web/src/styles.css` dark-mode
    /// `:root` + `@media (prefers-color-scheme: dark)`. Default
    /// when the OS reports dark appearance.
    pub const WEB_DARK: Self = Self {
        background: 0x0e0e10,
        foreground: 0xe8e8e8,
        muted_foreground: 0x8a8a8a,
        secondary: 0x161618,
        border: 0x2a2a2c,
        primary: 0xff8c42,
        success: 0x2ecc71,
        danger: 0xff5757,
        cyber_cyan: 0x6df0ff,
        cyber_pink: 0xff2a6d,
        cyber_amber: 0xff8c2a,
        agent_text: 0xc8d4ff,
        agent_muted: 0x8aa0d8,
        cyber_bg_deep: 0x0a0f1e,
    };

    /// Mirrors the web's light-mode `:root` block. Default when
    /// the OS reports light appearance.
    pub const WEB_LIGHT: Self = Self {
        background: 0xfafafa,
        foreground: 0x1a1a1a,
        muted_foreground: 0x6a6a6a,
        secondary: 0xffffff,
        border: 0xe3e3e3,
        primary: 0xd35400,
        success: 0x27ae60,
        danger: 0xc0392b,
        // Cyberpunk accents are visually identical in both modes
        // on the web (panels are dark-on-light by design); reuse
        // the dark-mode values.
        cyber_cyan: 0x6df0ff,
        cyber_pink: 0xff2a6d,
        cyber_amber: 0xff8c2a,
        agent_text: 0xc8d4ff,
        agent_muted: 0x8aa0d8,
        cyber_bg_deep: 0x0a0f1e,
    };

    /// Pure cyberpunk preset — applies the panel accents to the
    /// CORE tokens too, so the whole window picks up the neon
    /// theme. Useful for screen-recording demos and the
    /// "cyberpunk skin" toggle in a future settings panel.
    pub const CYBERPUNK_NEON: Self = Self {
        background: 0x0a0f1e,
        foreground: 0xc8d4ff,
        muted_foreground: 0x8aa0d8,
        secondary: 0x11193a,
        border: 0x1a2348,
        primary: 0x6df0ff,
        success: 0x6df0ff,
        danger: 0xff2a6d,
        cyber_cyan: 0x6df0ff,
        cyber_pink: 0xff2a6d,
        cyber_amber: 0xff8c2a,
        agent_text: 0xc8d4ff,
        agent_muted: 0x8aa0d8,
        cyber_bg_deep: 0x0a0f1e,
    };

    /// Greyscale preset — useful on screen-sharing calls where
    /// notification overlays + accent colours pull focus from
    /// whatever the operator is presenting. Foreground is at the
    /// brightness ramp the WCAG AA contrast checker likes against
    /// the dark background, primary stays a slightly-warm grey so
    /// the active nav still reads as "selected".
    pub const MONO: Self = Self {
        background: 0x111111,
        foreground: 0xe6e6e6,
        muted_foreground: 0x8e8e8e,
        secondary: 0x1a1a1a,
        border: 0x2c2c2c,
        primary: 0xc4c4c4,
        success: 0xa0a0a0,
        danger: 0xb8b8b8,
        cyber_cyan: 0xc4c4c4,
        cyber_pink: 0xb8b8b8,
        cyber_amber: 0xaaaaaa,
        agent_text: 0xe6e6e6,
        agent_muted: 0x8e8e8e,
        cyber_bg_deep: 0x0a0a0a,
    };

    /// High-contrast preset — pure black background + near-white
    /// foreground + saturated brand colour. Tuned for low-vision
    /// operators and bright-environment use (sun on the laptop
    /// screen). The cyberpunk accents stay vivid since they're
    /// already saturated; the core ramp is harder.
    pub const HIGH_CONTRAST: Self = Self {
        background: 0x000000,
        foreground: 0xffffff,
        muted_foreground: 0xb0b0b0,
        secondary: 0x0a0a0a,
        border: 0x404040,
        primary: 0xff8c42,
        success: 0x00ff00,
        danger: 0xff3030,
        cyber_cyan: 0x00f0ff,
        cyber_pink: 0xff2a6d,
        cyber_amber: 0xff8c2a,
        agent_text: 0xffffff,
        agent_muted: 0xb0b0b0,
        cyber_bg_deep: 0x000000,
    };

    /// Resolve a palette name (lower-snake-case) to a const ref.
    /// Unknown names return `None`. Used by the env-var picker
    /// and any future settings-panel dropdown.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "web_dark" => Some(Self::WEB_DARK),
            "web_light" => Some(Self::WEB_LIGHT),
            "cyberpunk_neon" => Some(Self::CYBERPUNK_NEON),
            "mono" => Some(Self::MONO),
            "high_contrast" => Some(Self::HIGH_CONTRAST),
            _ => None,
        }
    }

    /// All registered palette names in display order. Used by the
    /// future settings-panel dropdown so a UI can render every
    /// option without tracking the list separately.
    pub const ALL_NAMES: &'static [&'static str] = &[
        "web_dark",
        "web_light",
        "cyberpunk_neon",
        "mono",
        "high_contrast",
    ];
}

/// Env var that overrides the OS-appearance default. Recognised
/// values: any name accepted by [`Palette::by_name`].
pub const PALETTE_ENV: &str = "CRABCC_DESKTOP_PALETTE";

/// Apply the right palette to gpui-component's global theme.
/// Call from `main` after `gpui_component::init(cx)`.
///
/// 1. If `CRABCC_DESKTOP_PALETTE` is set and resolves, use that.
/// 2. Otherwise pick `WEB_DARK` / `WEB_LIGHT` per OS appearance.
pub fn install(cx: &mut App) {
    let palette = std::env::var(PALETTE_ENV)
        .ok()
        .and_then(|n| Palette::by_name(&n))
        .unwrap_or_else(|| {
            if Theme::global(cx).is_dark() {
                Palette::WEB_DARK
            } else {
                Palette::WEB_LIGHT
            }
        });
    apply(cx, palette);
}

/// Direct apply, bypassing the env-var + appearance picker. Used
/// by tests and by future settings-panel "preview" code.
pub fn install_with(cx: &mut App, palette: Palette) {
    apply(cx, palette);
}

fn apply(cx: &mut App, palette: Palette) {
    let theme = Theme::global_mut(cx);
    theme.background = rgb(palette.background).into();
    theme.foreground = rgb(palette.foreground).into();
    theme.muted_foreground = rgb(palette.muted_foreground).into();
    theme.secondary = rgb(palette.secondary).into();
    theme.border = rgb(palette.border).into();
    theme.primary = rgb(palette.primary).into();
    theme.success = rgb(palette.success).into();
    theme.danger = rgb(palette.danger).into();
}

/// Convenience wrapper — converts a `u32` palette token to an
/// `Hsla`. Per-route widgets that read cyberpunk accents directly
/// (e.g. `palette::cyber_cyan(theme)`) can use this without
/// importing `gpui::rgb` everywhere.
#[inline]
pub fn cyber(hex: u32) -> Hsla {
    rgb(hex).into()
}

// Backwards-compatible re-exports for the const-named accents
// shipped in the first slice (#356). Keep these so unconverted
// call sites compile without churn — point at the same hex
// values Palette::WEB_DARK uses.
pub const CYBER_CYAN: u32 = Palette::WEB_DARK.cyber_cyan;
pub const CYBER_PINK: u32 = Palette::WEB_DARK.cyber_pink;
pub const CYBER_AMBER: u32 = Palette::WEB_DARK.cyber_amber;
pub const AGENT_TEXT: u32 = Palette::WEB_DARK.agent_text;
pub const AGENT_MUTED: u32 = Palette::WEB_DARK.agent_muted;
pub const CYBER_BG_DEEP: u32 = Palette::WEB_DARK.cyber_bg_deep;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_round_trip() {
        // Pin the wire-name set so future renames break tests.
        // ALL_NAMES is the canonical list — every entry must
        // resolve via `by_name`; nothing else should.
        for name in Palette::ALL_NAMES {
            assert!(Palette::by_name(name).is_some(), "{name} should resolve",);
        }
        assert_eq!(Palette::ALL_NAMES.len(), 5);

        // Unknowns return None — `install` falls back to the
        // OS-appearance default in that case.
        assert!(Palette::by_name("WEB_DARK").is_none());
        assert!(Palette::by_name("oops").is_none());
        assert!(Palette::by_name("").is_none());
    }

    #[test]
    fn legacy_consts_match_web_dark() {
        // The pre-refactor const surface (CYBER_CYAN etc.) keeps
        // working; pin to WEB_DARK values so a future palette
        // edit doesn't silently drift the legacy import sites.
        assert_eq!(CYBER_CYAN, Palette::WEB_DARK.cyber_cyan);
        assert_eq!(CYBER_PINK, Palette::WEB_DARK.cyber_pink);
        assert_eq!(CYBER_AMBER, Palette::WEB_DARK.cyber_amber);
        assert_eq!(AGENT_TEXT, Palette::WEB_DARK.agent_text);
        assert_eq!(AGENT_MUTED, Palette::WEB_DARK.agent_muted);
        assert_eq!(CYBER_BG_DEEP, Palette::WEB_DARK.cyber_bg_deep);
    }
}
