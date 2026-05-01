use anyhow::Result;
use clap::{Parser, Subcommand};
use crabcc_chrome::{config, host, pair, serve, HOST_NAME};

#[derive(Debug, Parser)]
#[command(
    name = "crabcc-chrome",
    version,
    about = "Native-messaging host + stdio MCP bridge for the crabcc Chrome extension.",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Native-messaging host mode. Chrome launches this with framed
    /// JSON on stdin/stdout. The process connects to the running
    /// `serve` instance via TCP loopback and bidirectionally relays.
    Host,

    /// Long-lived bridge. Speaks MCP JSON-RPC 2.0 on stdin/stdout to
    /// an MCP client (e.g. Claude Code), and accepts a single `host`
    /// connection over TCP loopback. Translates MCP tool calls into
    /// the extension's RpcRequest envelope.
    Serve,

    /// Install the Chrome NativeMessagingHosts manifest and write a
    /// shared secret + connection config to ~/.crabcc/chrome.toml.
    Pair {
        /// Chrome extension ID (32-char `chrome-extension://<id>` value
        /// shown on `chrome://extensions` after loading the unpacked
        /// extension). The manifest's `allowed_origins` is pinned to
        /// this ID — Chrome rejects connectNative from any other.
        #[arg(long)]
        id: String,
        /// Browser flavour to install for. Defaults to chrome.
        #[arg(long, default_value = "chrome")]
        browser: pair::Browser,
        /// Overwrite an existing manifest without prompting.
        #[arg(long)]
        force: bool,
    },

    /// Remove the NativeMessagingHosts manifest and chrome.toml.
    Unpair {
        #[arg(long, default_value = "chrome")]
        browser: pair::Browser,
    },

    /// Print pairing status — host name, manifest path, allowed origins,
    /// secret presence — without exposing the secret itself.
    Status {
        #[arg(long, default_value = "chrome")]
        browser: pair::Browser,
    },
}

fn main() -> Result<()> {
    // Native-messaging hosts emit log noise on stdout to Chrome — avoid
    // tracing-subscriber's default formatter clobbering the JSON
    // protocol. We log to stderr; Chrome captures stderr separately.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CRABCC_CHROME_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Cmd::Host => host::run(),
        Cmd::Serve => serve::run(),
        Cmd::Pair { id, browser, force } => pair::install(&id, browser, force),
        Cmd::Unpair { browser } => pair::remove(browser),
        Cmd::Status { browser } => {
            let cfg = config::load_or_default();
            let manifest = pair::manifest_path(browser)?;
            println!("host name:        {HOST_NAME}");
            println!("manifest:         {}", manifest.display());
            println!("manifest exists:  {}", manifest.exists());
            println!("config:           {}", config::path()?.display());
            println!("secret set:       {}", !cfg.secret.is_empty());
            println!("listen port:      {}", cfg.port);
            Ok(())
        }
    }
}
