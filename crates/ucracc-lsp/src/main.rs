//! `ucracc-lsp` — a navigation + retrieval Language Server backed by the
//! crabcc symbol DB. Communicates over stdio (LSP standard).

use tower_lsp::{LspService, Server};
use ucracc_lsp::server::Backend;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("UCRACC_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
