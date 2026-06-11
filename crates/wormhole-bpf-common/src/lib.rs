#![no_std]

pub const RELAY_PORT: u16 = 4443;
/// Maximum new TCP connections per second per source IP before SYN packets are dropped.
pub const CONN_RATE_LIMIT: u32 = 100;
/// Rate-limit window length in nanoseconds (1 second).
pub const RATE_WINDOW_NS: u64 = 1_000_000_000;
/// Maximum number of source IPs tracked in the LRU BPF map.
pub const RATE_MAP_MAX_ENTRIES: u32 = 65536;

/// Per-source-IP connection rate state stored in the BPF LRU map.
///
/// `_pad` aligns `window_start_ns` to 8 bytes so the layout is identical
/// across the BPF target and the userspace loader.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct ConnState {
    pub count: u32,
    pub _pad: u32,
    pub window_start_ns: u64,
}
