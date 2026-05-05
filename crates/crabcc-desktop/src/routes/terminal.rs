//! Terminal route — embedded shell, alacritty_terminal-backed.
//!
//! First-cut implementation of issue #402. The grid model lives in
//! `crate::terminal::Terminal` (PTY + `alacritty_terminal::Term`); this
//! module is the GPUI view layer:
//!
//!   * `Render` walks the screen-grid lines and paints each as a row
//!     of characters using the project's theme tokens.
//!   * `on_key_down` translates GPUI keystrokes into terminal byte
//!     sequences — printable chars, Enter / Tab / Backspace, the four
//!     arrow keys, and Ctrl-letter shortcuts. Cursor movement /
//!     scrollback / selection are intentionally out of scope.
//!   * The route notifies on every key event so the next frame walks
//!     the freshly-mutated grid. (No frame-rate diffing yet — alacritty
//!     events drive cx.notify too, see `Self::pump_events`.)
//!
//! Visual fidelity to the Stitch mock at
//! `docs/desktop/stitch-refs/terminal-chat/01-screenshot.png` is
//! deliberately incomplete; that's the polish track tracked in #402.

use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::grid::Dimensions;
use gpui::{
    div, prelude::*, App, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    Render, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use crate::state::AppState;
use crate::terminal::Terminal;

/// One terminal route per dashboard tab. Owns its `Terminal` for its
/// entire lifetime — switching tabs and back keeps the existing shell
/// session alive (closing the route by some future "X" button is what
/// drops the Terminal and reaps the child via `Drop`).
pub struct TerminalRoute {
    #[allow(dead_code)] // wired so future `state`-driven features (e.g. `cwd` overlay) compose
    state: Entity<AppState>,
    terminal: Terminal,
    focus: FocusHandle,
}

impl TerminalRoute {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal = Terminal::spawn().expect("spawn embedded shell");
        let focus = cx.focus_handle();
        Self {
            state,
            terminal,
            focus,
        }
    }

    /// Translate a GPUI keystroke into the byte sequence a TTY expects.
    /// Returns `None` for keys we don't currently handle (function
    /// keys F1-F12 above, etc.) so the runtime can skip the write.
    fn keystroke_bytes(ev: &KeyDownEvent) -> Option<Vec<u8>> {
        let k = &ev.keystroke;
        let mods = &k.modifiers;

        // Ctrl-<letter> first — the test must come before the
        // printable-char branch since `key` for ctrl-c is just "c".
        if mods.control && !mods.alt && !mods.platform {
            let key = k.key.as_str();
            if key.len() == 1 {
                let ch = key.chars().next().unwrap().to_ascii_lowercase();
                if ch.is_ascii_alphabetic() {
                    // Ctrl-A → 0x01, Ctrl-Z → 0x1A.
                    return Some(vec![(ch as u8) - b'a' + 1]);
                }
            }
            // Ctrl-Space (NUL) — handy for some shells.
            if key == "space" {
                return Some(vec![0]);
            }
        }

        // Named keys. Match on `key`, not `key_char`, since `key_char`
        // is empty for Enter / Backspace etc.
        let bytes: &[u8] = match k.key.as_str() {
            "enter" => b"\r",
            "tab" => b"\t",
            "backspace" => b"\x7f",
            "escape" => b"\x1b",
            "delete" => b"\x1b[3~",
            "home" => b"\x1b[H",
            "end" => b"\x1b[F",
            "pageup" => b"\x1b[5~",
            "pagedown" => b"\x1b[6~",
            "up" => b"\x1b[A",
            "down" => b"\x1b[B",
            "right" => b"\x1b[C",
            "left" => b"\x1b[D",
            _ => &[],
        };
        if !bytes.is_empty() {
            return Some(bytes.to_vec());
        }

        // Printable character. Prefer `key_char` (covers shifted /
        // dead-key sequences), fall back to `key` for plain ASCII.
        let single_char_key = if k.key.len() == 1 {
            Some(k.key.as_str())
        } else {
            None
        };
        if let Some(ch) = k.key_char.as_deref().or(single_char_key) {
            if !ch.is_empty() {
                return Some(ch.as_bytes().to_vec());
            }
        }
        None
    }

    /// Drain alacritty events so the renderer reacts to title changes
    /// and child-exit notices. Title goes to a future window-title
    /// surface (see #402); ChildExit is logged for now and will become
    /// a "[ Restart ]" overlay in a follow-up.
    fn pump_events(&self, cx: &mut Context<Self>) {
        for event in self.terminal.drain_events() {
            match event {
                TermEvent::Title(t) => {
                    tracing::trace!(target: "crabcc_desktop::terminal", title = %t);
                }
                TermEvent::ChildExit(code) => {
                    tracing::info!(target: "crabcc_desktop::terminal", ?code, "shell exited");
                }
                TermEvent::Bell => {
                    tracing::trace!(target: "crabcc_desktop::terminal", "bell");
                }
                _ => {}
            }
        }
        cx.notify();
    }

    fn handle_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(bytes) = Self::keystroke_bytes(ev) {
            self.terminal.write(bytes);
            self.pump_events(cx);
        }
    }
}

impl Focusable for TerminalRoute {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        // Snapshot the grid into Strings so the lock isn't held across
        // GPUI element construction (which can be re-entrant via theme
        // accessors). The clone is O(rows × cols) chars per frame —
        // small numbers (24×80 = 1920 chars) at human cadence, so the
        // wall-clock cost is invisible until we wire scrollback.
        let (rows, cursor_line, cursor_col) = {
            let term = self.terminal.term.lock();
            let cols = term.columns();
            let lines = term.screen_lines();
            let mut out: Vec<String> = Vec::with_capacity(lines);
            let grid = term.grid();
            for line_idx in 0..lines {
                let mut s = String::with_capacity(cols);
                for col_idx in 0..cols {
                    let line = alacritty_terminal::index::Line(line_idx as i32);
                    let column = alacritty_terminal::index::Column(col_idx);
                    let cell = &grid[line][column];
                    // Replace null/zero cells with a single space so
                    // empty rows still take the same line height as
                    // populated ones (no layout jitter when output
                    // arrives).
                    s.push(if cell.c == '\0' || cell.c == ' ' {
                        ' '
                    } else {
                        cell.c
                    });
                }
                // Trim trailing spaces — saves both render width and
                // makes selecting / copying less awkward later.
                let trimmed = s.trim_end().to_string();
                out.push(trimmed);
            }
            let cursor = grid.cursor.point;
            (out, cursor.line.0, cursor.column.0)
        };

        // Header strip — mirrors the rest of the dashboard's route
        // header pattern (route title + muted breadcrumb).
        let header = h_flex()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .text_color(foreground)
                    .child(SharedString::new_static("Terminal")),
            )
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::new_static("· $SHELL · click to focus")),
            );

        // Body — one div per row, monospace, foreground colour from
        // theme. Cursor row gets a faint underline; this is a
        // placeholder for a real glyph-level cursor (issue #402 polish).
        let body = v_flex()
            .px_5()
            .py_2()
            .gap_0()
            .text_color(foreground)
            .children(rows.into_iter().enumerate().map(|(i, row)| {
                let is_cursor_line = i as i32 == cursor_line;
                let label = if row.is_empty() {
                    SharedString::new_static(" ")
                } else {
                    SharedString::from(row)
                };
                let mut row_div = div().font_family("JetBrains Mono").child(label);
                if is_cursor_line {
                    // Hint of where the cursor lives until we paint a
                    // real block-cursor element. Uses border_b instead
                    // of bg so reading the row content stays primary.
                    row_div = row_div.border_b_1().border_color(muted);
                    let _ = cursor_col; // suppress unused warning until cursor element lands
                }
                row_div
            }));

        v_flex()
            .id("terminal-route")
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}
