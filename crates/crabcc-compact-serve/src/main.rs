use std::{net::SocketAddr, sync::Arc, time::Instant};

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Parser)]
#[command(about = "crabcc compact-server: Tokio/axum ingress, Python ML handlers")]
struct Cli {
    /// Directory containing compress.py and enrich.py
    #[arg(long, env = "COMPACT_PYTHON_PATH", default_value = ".")]
    python_path: String,

    #[arg(long, env = "COMPACT_PORT", default_value = "8080")]
    port: u16,
}

#[derive(Clone)]
struct Ctx {
    python_path: Arc<String>,
    started: Arc<Instant>,
}

// ── wire types ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CompactReq {
    text: String,
    #[serde(default = "half")]
    ratio: f64,
}
fn half() -> f64 {
    0.5
}

#[derive(Serialize)]
struct CompactResp {
    compressed: String,
    original_tokens: usize,
    compressed_tokens: usize,
}

#[derive(Deserialize)]
struct EnrichReq {
    text: String,
    query: String,
}

#[derive(Serialize)]
struct EnrichResp {
    plan: String,
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Initialize the Python interpreter once. Must happen before any
    // Python::with_gil call — and before spawning threads that might
    // call into Python.
    pyo3::prepare_freethreaded_python();

    let ctx = Ctx {
        python_path: Arc::new(cli.python_path),
        started: Arc::new(Instant::now()),
    };
    let app = build_router(ctx);
    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    info!("listening on {addr}");
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

fn build_router(ctx: Ctx) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/compact", post(handle_compact))
        .route("/enrich", post(handle_enrich))
        .with_state(ctx)
}

// ── handlers ──────────────────────────────────────────────────────────────────

async fn handle_health(State(ctx): State<Ctx>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "uptime_s": ctx.started.elapsed().as_secs(),
    }))
}

async fn handle_compact(
    State(ctx): State<Ctx>,
    Json(req): Json<CompactReq>,
) -> Result<Json<CompactResp>, StatusCode> {
    let path = ctx.python_path.clone();
    tokio::task::spawn_blocking(move || py_compact(&path, &req.text, req.ratio))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn handle_enrich(
    State(ctx): State<Ctx>,
    Json(req): Json<EnrichReq>,
) -> Result<Json<EnrichResp>, StatusCode> {
    let path = ctx.python_path.clone();
    tokio::task::spawn_blocking(move || py_enrich(&path, &req.text, &req.query))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

// ── Python wrappers (hold GIL; always call from spawn_blocking) ───────────────

fn with_path<T>(
    py: Python<'_>,
    path: &str,
    f: impl FnOnce(Python<'_>) -> PyResult<T>,
) -> PyResult<T> {
    py.import_bound("sys")?
        .getattr("path")?
        .call_method1("insert", (0, path))?;
    f(py)
}

fn py_compact(python_path: &str, text: &str, ratio: f64) -> Result<CompactResp> {
    Python::with_gil(|py| {
        with_path(py, python_path, |py| {
            let r = py
                .import_bound("compress")?
                .call_method1("compact", (text, ratio))?;
            Ok(CompactResp {
                compressed: r.get_item("compressed")?.extract()?,
                original_tokens: r.get_item("original_tokens")?.extract()?,
                compressed_tokens: r.get_item("compressed_tokens")?.extract()?,
            })
        })
    })
    .map_err(|e| anyhow::anyhow!("py_compact: {e}"))
}

fn py_enrich(python_path: &str, text: &str, query: &str) -> Result<EnrichResp> {
    Python::with_gil(|py| {
        with_path(py, python_path, |py| {
            let plan: String = py
                .import_bound("enrich")?
                .call_method1("enrich", (text, query))?
                .extract()?;
            Ok(EnrichResp { plan })
        })
    })
    .map_err(|e| anyhow::anyhow!("py_enrich: {e}"))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use tower::ServiceExt as _;

    fn app() -> Router {
        let ctx = Ctx {
            python_path: Arc::new(".".into()),
            started: Arc::new(Instant::now()),
        };
        build_router(ctx)
    }

    #[tokio::test]
    async fn health_200_ok() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v["uptime_s"].as_u64().is_some());
    }

    #[tokio::test]
    async fn compact_missing_text_is_422() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compact")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"ratio": 0.5}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // axum rejects missing required field before reaching the handler
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
