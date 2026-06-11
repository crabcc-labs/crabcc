#![no_std]
#![no_main]

use aya_bpf::{
    bindings::xdp_action,
    macros::{map, xdp},
    maps::LruHashMap,
    programs::XdpContext,
};
use wormhole_bpf_common::{ConnState, CONN_RATE_LIMIT, RATE_MAP_MAX_ENTRIES, RATE_WINDOW_NS, RELAY_PORT};

#[map]
static CONN_RATE: LruHashMap<u32, ConnState> =
    LruHashMap::with_max_entries(RATE_MAP_MAX_ENTRIES, 0);

/// XDP entry point.  All errors fall back to XDP_PASS so legitimate traffic is
/// never accidentally dropped due to a parsing bug.
#[xdp]
pub fn wormhole_xdp(ctx: XdpContext) -> u32 {
    match try_xdp(&ctx) {
        Ok(action) => action,
        Err(_) => xdp_action::XDP_PASS,
    }
}

/// Read `size_of::<T>()` bytes at `offset` from the packet data, checking bounds.
#[inline(always)]
unsafe fn ptr_at<T>(ctx: &XdpContext, offset: usize) -> Result<*const T, ()> {
    let start = ctx.data();
    let end = ctx.data_end();
    let len = core::mem::size_of::<T>();
    if start + offset + len > end {
        return Err(());
    }
    Ok((start + offset) as *const T)
}

fn try_xdp(ctx: &XdpContext) -> Result<u32, ()> {
    const ETH_HDR_LEN: usize = 14;
    const ETH_P_IP: u16 = 0x0800;
    const IPPROTO_TCP: u8 = 6;

    // Only handle IPv4.
    let eth_proto = u16::from_be(unsafe { *ptr_at::<u16>(ctx, 12)? });
    if eth_proto != ETH_P_IP {
        return Ok(xdp_action::XDP_PASS);
    }

    // Only handle TCP.
    let proto = unsafe { *ptr_at::<u8>(ctx, ETH_HDR_LEN + 9)? };
    if proto != IPPROTO_TCP {
        return Ok(xdp_action::XDP_PASS);
    }

    // IHL gives us the variable-length IP header size.
    let ihl = (unsafe { *ptr_at::<u8>(ctx, ETH_HDR_LEN)? } & 0x0f) as usize * 4;
    let src_ip = u32::from_be(unsafe { *ptr_at::<u32>(ctx, ETH_HDR_LEN + 12)? });

    let tcp_offset = ETH_HDR_LEN + ihl;
    let dst_port = u16::from_be(unsafe { *ptr_at::<u16>(ctx, tcp_offset + 2)? });
    if dst_port != RELAY_PORT {
        return Ok(xdp_action::XDP_PASS);
    }

    // Only rate-limit SYN packets (flags byte at TCP+13, SYN = bit 1).
    let flags = unsafe { *ptr_at::<u8>(ctx, tcp_offset + 13)? };
    if flags & 0x02 == 0 {
        return Ok(xdp_action::XDP_PASS);
    }

    let now_ns = unsafe { aya_bpf::helpers::bpf_ktime_get_ns() };

    if let Some(state) = unsafe { CONN_RATE.get_ptr_mut(&src_ip) } {
        let s = unsafe { &mut *state };
        if now_ns.saturating_sub(s.window_start_ns) < RATE_WINDOW_NS {
            s.count += 1;
            if s.count > CONN_RATE_LIMIT {
                return Ok(xdp_action::XDP_DROP);
            }
        } else {
            // Window expired — start a fresh window.
            s.window_start_ns = now_ns;
            s.count = 1;
        }
    } else {
        let _ = CONN_RATE.insert(
            &src_ip,
            &ConnState {
                count: 1,
                _pad: 0,
                window_start_ns: now_ns,
            },
            0,
        );
    }

    Ok(xdp_action::XDP_PASS)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
