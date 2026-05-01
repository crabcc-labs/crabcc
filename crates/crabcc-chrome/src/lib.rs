//! crabcc-chrome — native-messaging host + stdio MCP bridge for the
//! crabcc Chrome extension (issue #184, Phase 0.5+).
//!
//! Architecture:
//!
//! ```text
//!   MCP client ──stdio──▶ crabcc-chrome serve ──TCP loopback──▶ crabcc-chrome host
//!                                                                       ▲
//!                                                                       │ chrome
//!                                                                       │ native
//!                                                                       │ messaging
//!                                                                       │ (4-byte LE
//!                                                                       │  length +
//!                                                                       │  JSON)
//!                                                                       │
//!                                                                  Chrome MV3 extension
//! ```
//!
//! Modules are deliberately small and self-contained — the binary is
//! intended to be auditable in a single sitting. See [`host`] for the
//! Chrome-launched mode and [`serve`] for the long-lived bridge.

pub mod config;
pub mod framing;
pub mod host;
pub mod pair;
pub mod serve;

/// Native-messaging host name. Must match the manifest installed by
/// [`pair::install`] and the `connectNative` argument the extension uses.
pub const HOST_NAME: &str = "com.crabcc.chrome";

/// Bumped whenever the wire envelope between extension and bridge
/// changes shape. The extension reads this off the `hello` message and
/// refuses to attach if the major doesn't match.
pub const WIRE_VERSION: u32 = 1;

#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::Mutex;
    /// Shared lock for tests that mutate `$CRABCC_CHROME_CONFIG` / `$HOME`.
    /// Without this guard, parallel tests race on the env vars.
    pub(crate) static ENV_GUARD: Mutex<()> = Mutex::new(());
}
