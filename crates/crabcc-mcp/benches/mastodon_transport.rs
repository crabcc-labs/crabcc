//! Mastodon transport micro-benchmarks.
//!
//! Run: cargo bench -p crabcc-mcp --features bench --bench mastodon_transport

use std::hint::black_box;

use crabcc_mcp::mastodon;
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_validate_token(c: &mut Criterion) {
    let valid = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2";
    c.bench_function("validate_token_valid", |b| {
        b.iter(|| black_box(mastodon::validate_token_for_bench(black_box(valid))));
    });
}

fn bench_sanitize_key(c: &mut Criterion) {
    let key = "release:v2.0.0-42_test";
    c.bench_function("sanitize_idem_key", |b| {
        b.iter(|| black_box(mastodon::sanitize_idem_key_for_bench(black_box(key))));
    });
}

fn bench_encode_hashtag(c: &mut Criterion) {
    let tag = "../../admin%2f%2e";
    c.bench_function("encode_hashtag", |b| {
        b.iter(|| black_box(mastodon::encode_hashtag_for_bench(black_box(tag))));
    });
}

fn bench_sse_format(c: &mut Criterion) {
    let data = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"sym"}]}}"#;
    c.bench_function("sse_event_format", |b| {
        b.iter(|| {
            black_box(mastodon::sse_event_with_id_for_bench(
                "message",
                black_box(data),
                42,
            ))
        });
    });
}

fn bench_gzip(c: &mut Criterion) {
    use crabcc_mcp::compress_gzip_for_bench;
    let payload: Vec<u8> = (0..10)
        .flat_map(|i| {
            format!("id: {i}\nevent: message\ndata: {{\"result\":{{\"count\":{i}}}}}\n\n")
                .into_bytes()
        })
        .collect();
    c.bench_function("gzip_sse_stream", |b| {
        b.iter_with_large_drop(|| {
            let compressed = compress_gzip_for_bench(black_box(payload.as_slice()));
            black_box(compressed)
        });
    });
}

fn bench_mastodon_dispatch(c: &mut Criterion) {
    std::env::set_var("MASTODON_TOKEN", "abcdef1234567890abcd");

    c.bench_function("mastodon_post_validation", |b| {
        let args = serde_json::json!({"text": "benchmark post"});
        b.iter(|| {
            let result = mastodon::dispatch("mastodon.post", black_box(&args));
            black_box(result)
        });
    });

    c.bench_function("mastodon_read_validation", |b| {
        let args = serde_json::json!({"timeline": "home", "limit": 5});
        b.iter(|| {
            let result = mastodon::dispatch("mastodon.read", black_box(&args));
            black_box(result)
        });
    });

    std::env::remove_var("MASTODON_TOKEN");
}

criterion_group!(
    benches,
    bench_validate_token,
    bench_sanitize_key,
    bench_encode_hashtag,
    bench_sse_format,
    bench_gzip,
    bench_mastodon_dispatch,
);
criterion_main!(benches);
