// Default: mimalloc (~10-30% faster small-alloc vs system malloc).
// Build with --no-default-features --features hardened-alloc for guard-page
// hardened_malloc (UAF/heap-spray protection, ~10% overhead).
#[cfg(feature = "mimalloc-alloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Result;
use clap::Parser;
use std::time::Duration;
use tracing::{error, info};

mod cmd;
mod keys;
mod session;

use keys::NodeKeys;
use session::{expand_tilde, run_session};

#[derive(Parser)]
#[command(name = "wormhole-node")]
struct Cli {
    #[arg(long, env = "WORMHOLE_RELAY_URL", default_value = "ws://127.0.0.1:4443/wormhole/v1")]
    relay_url: String,

    #[arg(long, env = "WORMHOLE_KEYS_FILE", default_value = "~/.crabcc/wormhole/node-keys.bin")]
    keys_file: String,

    /// Hex-encoded operator static pub key (optional; enables IK pre-shared pub)
    #[arg(long, env = "WORMHOLE_OP_STATIC_PUB")]
    op_static_pub: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let keys_path = expand_tilde(&cli.keys_file);
    let wormhole_dir = keys_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();

    let keys = NodeKeys::load_or_generate(&keys_path)?;
    info!(node_id = encode_hex(&keys.node_id), "node keys loaded");

    let op_static_pub: Option<[u8; 32]> = cli
        .op_static_pub
        .as_deref()
        .map(decode_hex32)
        .transpose()?;

    let mut backoff = Duration::from_secs(1);
    loop {
        match run_session(&cli.relay_url, &keys, op_static_pub, &wormhole_dir).await {
            Ok(()) => {
                info!("session ended cleanly; reconnecting");
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                error!("session error: {e:#}; reconnecting in {}s", backoff.as_secs());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn encode_hex(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex32(s: &str) -> anyhow::Result<[u8; 32]> {
    if s.len() != 64 {
        anyhow::bail!("op-static-pub must be 64 hex chars, got {}", s.len());
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or_else(|| anyhow::anyhow!("invalid hex char"))?;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or_else(|| anyhow::anyhow!("invalid hex char"))?;
        out[i] = ((hi as u8) << 4) | (lo as u8);
    }
    Ok(out)
}
