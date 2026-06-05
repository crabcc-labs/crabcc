use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct CompactResponse {
    pub compressed: String,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
}

#[derive(Debug, Deserialize)]
pub struct EnrichResponse {
    pub plan: String,
}

#[derive(Serialize)]
struct CompactRequest<'a> {
    text: &'a str,
    ratio: f32,
}

#[derive(Serialize)]
struct EnrichRequest<'a> {
    text: &'a str,
    query: &'a str,
}

pub fn compact(
    endpoint: &str,
    text: &str,
    ratio: f32,
    timeout_ms: u64,
) -> anyhow::Result<CompactResponse> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let url = format!("{endpoint}/compact");
    let resp = client
        .post(&url)
        .json(&CompactRequest { text, ratio })
        .send()?
        .error_for_status()?;
    Ok(resp.json()?)
}

pub fn enrich(
    endpoint: &str,
    text: &str,
    query: &str,
    timeout_ms: u64,
) -> anyhow::Result<EnrichResponse> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let url = format!("{endpoint}/enrich");
    let resp = client
        .post(&url)
        .json(&EnrichRequest { text, query })
        .send()?
        .error_for_status()?;
    Ok(resp.json()?)
}

pub fn health(endpoint: &str, timeout_ms: u64) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let url = format!("{endpoint}/health");
    Ok(client.get(&url).send()?.error_for_status()?.json()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_returns_err_on_unreachable_endpoint() {
        let result = compact("http://127.0.0.1:1", "hello world", 0.5, 500);
        assert!(result.is_err());
    }

    #[test]
    fn enrich_returns_err_on_unreachable_endpoint() {
        let result = enrich("http://127.0.0.1:1", "code", "fix auth", 500);
        assert!(result.is_err());
    }

    #[test]
    fn health_returns_err_on_unreachable_endpoint() {
        let result = health("http://127.0.0.1:1", 500);
        assert!(result.is_err());
    }
}
