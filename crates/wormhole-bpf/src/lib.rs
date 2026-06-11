//! Userspace loader for the `wormhole-bpf` XDP rate-limiter.
//!
//! # Build process
//!
//! The XDP program lives in `bpf/` — a **separate** Cargo project that targets
//! `bpfel-unknown-none` (little-endian BPF).  It is **not** part of the
//! workspace so that the BPF-specific toolchain config (panic = "abort",
//! no_std, aya-bpf) does not pollute the host build.
//!
//! Typical build flow:
//!
//! ```text
//! # 1. Install the BPF target (once):
//! rustup target add bpfel-unknown-none
//!
//! # 2. Compile the eBPF program:
//! cd crates/wormhole-bpf/bpf
//! cargo build --target bpfel-unknown-none --release
//!
//! # 3. Build the userspace loader (this crate) — build.rs embeds the bytes:
//! cd ..
//! cargo build --release
//! ```
//!
//! `build.rs` (not yet wired) should compile step 2 automatically and write
//! the ELF bytes into `OUT_DIR/wormhole-bpf-ebpf` so that `src/main.rs` can
//! embed them with `include_bytes_aligned!`.
//!
//! # Design
//!
//! The XDP program performs two fast-path checks before userspace sees a packet:
//!
//! 1. **Port filter** — non-TCP traffic and TCP traffic not destined for
//!    `RELAY_PORT` (default 4443) is passed through without inspection.
//! 2. **Per-source-IP SYN rate limit** — SYN packets are counted per source
//!    IP in a BPF `LruHashMap`.  If a source exceeds `CONN_RATE_LIMIT`
//!    (100 SYNs/s) within a 1-second window, subsequent SYNs are dropped at
//!    the NIC, before they consume a socket, thread, or Tokio task.

#[cfg(target_os = "linux")]
pub use aya;
pub use wormhole_bpf_common as common;
