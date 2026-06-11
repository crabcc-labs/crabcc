use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "wormhole-bpf",
    about = "Attach wormhole XDP rate-limiter to a network interface"
)]
struct Cli {
    /// Network interface to attach to (e.g. eth0, ens3)
    #[arg(long, default_value = "eth0")]
    iface: String,
    /// Use SKB mode (slower but works without native XDP driver support)
    #[arg(long)]
    skb_mode: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _cli = Cli::parse();
    env_logger::init();

    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("wormhole-bpf only runs on Linux (aya requires the BPF syscall)");

    #[cfg(target_os = "linux")]
    run_linux(_cli).await
}

#[cfg(target_os = "linux")]
async fn run_linux(cli: Cli) -> Result<()> {
    use aya::{include_bytes_aligned, programs::Xdp, programs::XdpFlags, Bpf};
    use tokio::signal;

    // The BPF bytecode is embedded at compile time by build.rs.
    // Until build.rs is wired up this binary exits with an actionable error.
    //
    // To enable:
    //   1. `rustup target add bpfel-unknown-none`
    //   2. `cd crates/wormhole-bpf/bpf && cargo build --target bpfel-unknown-none --release`
    //   3. Wire build.rs to compile bpf/ and write bytes to OUT_DIR.
    //   4. Replace this bail! with:
    //        let bpf_bytes = include_bytes_aligned!(
    //            concat!(env!("OUT_DIR"), "/wormhole-bpf-ebpf")
    //        );
    //        let mut bpf = Bpf::load(bpf_bytes)?;
    //        let prog: &mut Xdp = bpf.program_mut("wormhole_xdp").unwrap().try_into()?;
    //        prog.load()?;
    //        let flags = if cli.skb_mode { XdpFlags::SKB_MODE } else { XdpFlags::default() };
    //        prog.attach(&cli.iface, flags)?;
    //        signal::ctrl_c().await?;
    let _ = cli;
    anyhow::bail!(
        "wormhole-bpf is not yet compiled — \
         run `cargo build --target bpfel-unknown-none --release` inside \
         crates/wormhole-bpf/bpf/ and wire build.rs to embed the output. \
         See crates/wormhole-bpf/src/lib.rs for full build instructions."
    );
}
