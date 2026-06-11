use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "wormhole-op",
    about = "wormhole operator CLI — inspect relay health and session logs",
    long_about = "Wave 3 operator tooling for wormhole-relay.\n\
                  \n\
                  spawn (Noise_IK command dispatch) is planned for Wave 3 once the\n\
                  full initiator handshake is implemented in wormhole-op/src/lib.rs."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// HTTP health check against the relay.
    Health {
        /// Relay base URL (e.g. http://localhost:4443)
        #[arg(long, env = "WORMHOLE_RELAY_URL")]
        relay: String,
    },
    /// Fetch the append-only replay log for a node.
    Replay {
        /// Relay base URL
        #[arg(long, env = "WORMHOLE_RELAY_URL")]
        relay: String,
        /// Node ID as lowercase hex (64 chars = 32 bytes)
        node_id: String,
        /// Return frames from this sequence number onward
        #[arg(long, default_value = "0")]
        from: u64,
        /// Bearer token for replay auth (RELAY_TOKEN on the relay side)
        #[arg(long, env = "RELAY_TOKEN")]
        token: Option<String>,
    },
    /// Fire-and-forget spawn on a remote node via Noise_IK relay channel.
    /// NOT YET IMPLEMENTED — requires Wave 3 Noise_IK initiator in lib.rs.
    Spawn {
        #[arg(long, env = "WORMHOLE_RELAY_URL")]
        relay: String,
        /// Target node ID as lowercase hex
        #[arg(long, env = "WORMHOLE_NODE_ID")]
        node_id: String,
        /// Program to run on the remote node
        program: String,
        /// Arguments to pass
        args: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Health { relay } => {
            let url = format!("{relay}/wormhole/v1/health");
            let body = reqwest::get(&url)
                .await
                .with_context(|| format!("GET {url}"))?
                .text()
                .await
                .context("read response body")?;
            print!("{body}");
        }
        Cmd::Replay { relay, node_id, from, token } => {
            let url = format!("{relay}/wormhole/v1/replay/{node_id}?from={from}");
            let client = reqwest::Client::new();
            let mut req = client.get(&url);
            if let Some(tok) = token {
                req = req.header("authorization", format!("Bearer {tok}"));
            }
            let resp = req.send().await.with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            let body = resp.text().await.context("read response body")?;
            if !status.is_success() {
                anyhow::bail!("{status}: {body}");
            }
            print!("{body}");
        }
        Cmd::Spawn { .. } => {
            anyhow::bail!(
                "spawn requires a Noise_IK handshake with the target node — \
                 not yet implemented (see docs/WORMHOLE.md §11 Wave 3 Tasks 7-8)"
            );
        }
    }
    Ok(())
}
