//! Embedded-terminal core. Wraps `alacritty_terminal::Term` + the
//! shipped `tty` + `event_loop` machinery, exposes a small, GPUI-
//! friendly facade:
//!
//!   * `Terminal::spawn(rows, cols)` boots a shell, returns a handle
//!     that owns the term state, the EventLoop sender, and the join
//!     handle for the reader thread.
//!   * `Terminal::write(bytes)` posts user input to the PTY (handles
//!     keystrokes from the GPUI `key_down` path).
//!   * `Terminal::resize(rows, cols, cell_w, cell_h)` re-sizes both
//!     the PTY and the alacritty grid in a single call.
//!   * `Terminal::with_term(|t| …)` gives the renderer locked access
//!     to the grid for the duration of one paint.
//!
//! Architecturally identical to `zed/crates/terminal/`; cut down to
//! the surface area we currently need (no selection, no link
//! detection, no scroll-region adjust, no OSC handlers — those land
//! in follow-ups per issue #402).
//!
//! The reader thread is started by `EventLoop::spawn` (alacritty's
//! own polling loop, single-threaded, drains the pty into the term
//! state) — we don't run our own reader.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{test::TermSize, Config, Term};
use alacritty_terminal::tty::{self, Options as TtyOptions, Shell};
use anyhow::{Context, Result};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

/// Default cell metrics until the renderer measures real glyph
/// dimensions. Used for the very first PTY size handshake; the next
/// `resize()` call replaces these with measured pixel sizes.
pub const DEFAULT_CELL_WIDTH_PX: u16 = 8;
pub const DEFAULT_CELL_HEIGHT_PX: u16 = 16;

/// Initial grid size for a freshly spawned terminal. Re-sized on the
/// first GPUI layout pass.
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 80;

/// Public facade owned by `TerminalRoute` (one per visible route).
pub struct Terminal {
    /// Locked term state. Reader thread (alacritty's `EventLoop`)
    /// holds this; the GPUI renderer locks it briefly to walk the
    /// grid each paint.
    pub term: Arc<FairMutex<Term<NotifyProxy>>>,
    /// `EventLoop` channel — `Msg::Input(bytes)` for user keystrokes,
    /// `Msg::Resize(WindowSize)` for size changes, `Msg::Shutdown`
    /// at drop time.
    sender: EventLoopSender,
    /// `recv` end of the proxy that surfaces `event::Event`s out of
    /// alacritty (Title, Bell, child exit, …). Polled from the
    /// renderer / state on each frame.
    events_rx: std_mpsc::Receiver<Event>,
    /// Reader thread handle. Joined on drop after Shutdown is sent.
    reader: Option<
        JoinHandle<(
            EventLoop<tty::Pty, NotifyProxy>,
            alacritty_terminal::event_loop::State,
        )>,
    >,
}

/// Forwards `EventListener` events into a plain mpsc channel so the
/// renderer can poll them on the GPUI side without holding any
/// alacritty internals. Cheap to clone (Sender) — we hand one to the
/// EventLoop and keep the receiver in `Terminal`.
#[derive(Clone)]
pub struct NotifyProxy(std_mpsc::Sender<Event>);

impl EventListener for NotifyProxy {
    fn send_event(&self, event: Event) {
        // Best-effort: if the renderer side has disconnected we just
        // drop the event. The terminal will keep working; only the
        // window-title / bell / exit-notification surfaces are lost.
        let _ = self.0.send(event);
    }
}

impl Terminal {
    /// Spawn a fresh shell at `DEFAULT_ROWS x DEFAULT_COLS`. Caller
    /// re-sizes on the first layout pass.
    pub fn spawn() -> Result<Self> {
        let (event_tx, event_rx) = std_mpsc::channel::<Event>();
        let proxy = NotifyProxy(event_tx);

        // Detect $SHELL; fall back to /bin/zsh on macOS, /bin/sh
        // elsewhere. POSIX login shells expect `-` prefix in argv[0]
        // for "this is your login shell" semantics, but we're not
        // doing a full login — interactive (-i) without a leading
        // dash is the standard "embedded terminal" call.
        let shell = std::env::var("SHELL").ok().or_else(|| {
            // Reasonable cross-platform default.
            #[cfg(target_os = "macos")]
            {
                Some("/bin/zsh".to_string())
            }
            #[cfg(not(target_os = "macos"))]
            {
                Some("/bin/sh".to_string())
            }
        });
        let opts = TtyOptions {
            shell: shell.map(|s| Shell::new(s, vec!["-i".into()])),
            working_directory: std::env::current_dir().ok(),
            drain_on_exit: false,
            env: Default::default(),
            #[cfg(target_os = "windows")]
            escape_args: false,
        };

        let size = WindowSize {
            num_lines: DEFAULT_ROWS,
            num_cols: DEFAULT_COLS,
            cell_width: DEFAULT_CELL_WIDTH_PX,
            cell_height: DEFAULT_CELL_HEIGHT_PX,
        };

        let pty = tty::new(&opts, size, /* window_id = */ 0).context("spawn shell pty")?;

        let term_size = TermSize::new(DEFAULT_COLS as usize, DEFAULT_ROWS as usize);
        let term = Term::new(Config::default(), &term_size, proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(
            term.clone(),
            proxy,
            pty,
            /* drain_on_exit = */ false,
            /* ref_test = */ false,
        )
        .context("alacritty EventLoop::new")?;
        let sender = event_loop.channel();
        let reader = event_loop.spawn();

        Ok(Self {
            term,
            sender,
            events_rx: event_rx,
            reader: Some(reader),
        })
    }

    /// Forward keystroke bytes to the PTY. The caller (the
    /// `key_down` handler) is responsible for translating GPUI
    /// `Keystroke` → terminal byte sequence (e.g. arrow keys → CSI
    /// `\x1b[A`/`\x1b[B`/…).
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    /// Re-size both the alacritty grid and the underlying PTY in one
    /// call. Cell pixel dimensions matter for SIGWINCH-aware programs
    /// (less, htop) that ask the kernel for the window size in pixels
    /// — pass the real measured values from the renderer.
    pub fn resize(&self, rows: u16, cols: u16, cell_w_px: u16, cell_h_px: u16) {
        // Update the term grid first (CPU-only, cheap), then notify
        // the EventLoop so it forwards to the PTY (blocking syscall).
        {
            let term_size = TermSize::new(cols as usize, rows as usize);
            self.term.lock().resize(term_size);
        }
        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_w_px,
            cell_height: cell_h_px,
        };
        let _ = self.sender.send(Msg::Resize(window_size));
    }

    /// Drain queued `event::Event`s. The renderer calls this once per
    /// frame and reacts to:
    ///   * `Event::Title(s)` — update the window title.
    ///   * `Event::ChildExit(code)` — show the exit-status overlay.
    ///   * `Event::Bell` — flash the title bar (TODO).
    pub fn drain_events(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events_rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Tell the EventLoop to stop polling; reader thread exits
        // cleanly, child shell receives SIGHUP via PTY close.
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(handle) = self.reader.take() {
            // Don't block the GPUI thread — the loop should drain
            // within a few ms; if it doesn't, we've got bigger
            // problems than a leaked thread on shutdown.
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PTY spawn + Drop must complete without blocking forever. If
    /// the EventLoop's Shutdown message ever races the reader thread
    /// in a way that wedges, this test catches it.
    #[test]
    #[cfg(unix)]
    fn spawn_then_drop_terminates_cleanly() {
        let term = Terminal::spawn().expect("spawn");
        // No interaction at all — just drop.
        drop(term);
    }

    #[test]
    #[cfg(unix)]
    fn resize_does_not_panic() {
        let term = Terminal::spawn().expect("spawn");
        term.resize(40, 120, 8, 16);
        // Lock briefly to confirm the grid was actually resized.
        let t = term.term.lock();
        use alacritty_terminal::grid::Dimensions;
        assert_eq!(t.columns(), 120);
        assert_eq!(t.screen_lines(), 40);
    }

    #[test]
    #[cfg(unix)]
    fn write_some_bytes_does_not_panic() {
        let term = Terminal::spawn().expect("spawn");
        term.write(b"echo hello\n".to_vec());
        // Give the EventLoop a beat to forward to the pty + read
        // back the echo. This is a smoke test, not a content
        // assertion — content flakes under shell-startup noise.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _events = term.drain_events();
    }
}
