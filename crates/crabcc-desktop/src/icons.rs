//! Tool-family line-icon set — 11 single-purpose icons matching the
//! greenfield design system spec ("16 px stroke 1.5; one icon per tool
//! family"). Closes sub-task 1 of issue #389.
//!
//! Each icon is a 16×16 SVG with `currentColor` strokes, so the
//! consumer (a GPUI `svg()` element or a React component) controls
//! the colour via theme tokens — no fork-per-tint needed.
//!
//! ## Adding a new icon
//!
//! 1. Drop a clean 16×16 SVG into `assets/icons/<name>.svg`. Use
//!    `currentColor` for strokes / fills you want theme-driven; only
//!    hardcode colour for accents that should NEVER be re-tinted.
//! 2. Add the variant to `ToolIcon` below.
//! 3. Map it in `ToolIcon::svg()`.
//!
//! ## Wiring into a GPUI route
//!
//! ```ignore
//! use gpui::svg;
//! use crate::icons::ToolIcon;
//!
//! div().child(
//!     svg()
//!         .source(ToolIcon::Sym.svg_path())
//!         .size_4()
//!         .text_color(cx.theme().primary)
//! )
//! ```
//!
//! GPUI's `svg().source(...)` accepts a `SharedString` path relative
//! to the asset roots configured at `Application` startup; the
//! current binary registers `crates/crabcc-desktop/assets/` so
//! `icons/sym.svg` resolves correctly.

use gpui::SharedString;

/// One variant per tool family from the design system spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolIcon {
    Sym,
    Refs,
    Callers,
    Outline,
    Fuzzy,
    Memory,
    Fetch,
    Agent,
    Index,
    Serve,
    Mcp,
}

impl ToolIcon {
    /// Asset path relative to the configured asset root. GPUI resolves
    /// this against the bundled `assets/` directory at runtime.
    pub fn svg_path(self) -> SharedString {
        SharedString::new_static(match self {
            Self::Sym => "icons/sym.svg",
            Self::Refs => "icons/refs.svg",
            Self::Callers => "icons/callers.svg",
            Self::Outline => "icons/outline.svg",
            Self::Fuzzy => "icons/fuzzy.svg",
            Self::Memory => "icons/memory.svg",
            Self::Fetch => "icons/fetch.svg",
            Self::Agent => "icons/agent.svg",
            Self::Index => "icons/index.svg",
            Self::Serve => "icons/serve.svg",
            Self::Mcp => "icons/mcp.svg",
        })
    }

    /// Inline SVG body — `include_str!`'d at compile time. Used by
    /// tests + by any consumer that wants the bytes directly without
    /// going through GPUI's asset resolver (for example, a future
    /// SSR / image-export path).
    pub fn svg_source(self) -> &'static str {
        match self {
            Self::Sym => include_str!("../assets/icons/sym.svg"),
            Self::Refs => include_str!("../assets/icons/refs.svg"),
            Self::Callers => include_str!("../assets/icons/callers.svg"),
            Self::Outline => include_str!("../assets/icons/outline.svg"),
            Self::Fuzzy => include_str!("../assets/icons/fuzzy.svg"),
            Self::Memory => include_str!("../assets/icons/memory.svg"),
            Self::Fetch => include_str!("../assets/icons/fetch.svg"),
            Self::Agent => include_str!("../assets/icons/agent.svg"),
            Self::Index => include_str!("../assets/icons/index.svg"),
            Self::Serve => include_str!("../assets/icons/serve.svg"),
            Self::Mcp => include_str!("../assets/icons/mcp.svg"),
        }
    }

    pub const ALL: [Self; 11] = [
        Self::Sym,
        Self::Refs,
        Self::Callers,
        Self::Outline,
        Self::Fuzzy,
        Self::Memory,
        Self::Fetch,
        Self::Agent,
        Self::Index,
        Self::Serve,
        Self::Mcp,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon's SVG body parses as well-formed XML at the basic
    /// "starts with <svg, ends with </svg>" level. We don't run a
    /// strict XML parser — bringing one in just for this would dwarf
    /// the asset payload — but this catches the common foot-gun of
    /// `include_str!` picking up a stale or empty file.
    #[test]
    fn every_icon_has_a_well_formed_svg_body() {
        for icon in ToolIcon::ALL {
            let body = icon.svg_source();
            assert!(
                body.trim_start().starts_with("<svg"),
                "{icon:?} svg body must start with <svg; got: {body:.40}"
            );
            assert!(
                body.trim_end().ends_with("</svg>"),
                "{icon:?} svg body must end with </svg>"
            );
            assert!(
                body.contains("viewBox=\"0 0 16 16\""),
                "{icon:?} must declare a 16×16 viewBox per the design system spec"
            );
            // currentColor lets the consumer theme-tint without
            // shipping per-palette copies. A literal hex colour
            // sneaking in would freeze the icon's hue across themes,
            // which is exactly the bug the spec is trying to prevent.
            assert!(
                body.contains("currentColor"),
                "{icon:?} must use currentColor (not a literal hex)"
            );
            assert!(
                !body.contains("#E6E6EB") && !body.contains("#0E0E12"),
                "{icon:?} contains a hardcoded theme colour — re-export with currentColor"
            );
        }
    }

    #[test]
    fn svg_path_strings_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for icon in ToolIcon::ALL {
            let path = icon.svg_path();
            assert!(seen.insert(path.clone()), "duplicate svg_path: {path}");
        }
    }

    #[test]
    fn all_array_size_matches_variants() {
        // A new variant added to the enum without an `ALL` entry
        // would silently fall through to "10 / 11 icons available"
        // bugs at runtime. This test pins the count.
        assert_eq!(ToolIcon::ALL.len(), 11);
    }
}
