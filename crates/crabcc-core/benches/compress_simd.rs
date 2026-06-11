// FSST codec throughput benchmark.
//
// Measures Codec::decompress end-to-end, which includes:
//   1. codes_in_range safety scan (scalar fallback or AVX2 fast path)
//   2. fsst-rs inner decompression loop
//
// On x86_64 with AVX2, the codes_in_range scan processes 32 bytes per
// iteration (vs 1 in the scalar path). The delta on a corpus with no escape
// bytes (typical for Rust signatures) is measurable at large batch sizes.
//
// Run:
//   cargo bench -p crabcc-core --features bench --bench compress_simd
//   cargo bench -p crabcc-core --features bench --bench compress_simd -- --warm-up-time 2

use crabcc_core::compress::Codec;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

const TEMPLATES: &[&str] = &[
    "fn get_user_profile(id: u64) -> Result<UserProfile, Error>",
    "fn authenticate(token: &str, secret: &[u8]) -> bool",
    "async fn handle_request(req: Request<Body>) -> Response<Body>",
    "impl Debug for Codec { fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result }",
    "pub struct Symbol { pub name: String, pub kind: SymbolKind, pub file: String }",
    "fn codes_in_range(encoded: &[u8], n_symbols: usize) -> bool",
    "pub fn compress(&self, plain: &[u8]) -> Vec<u8>",
    "impl<T: Send + Sync> Store<T> { pub fn open(path: &Path) -> Result<Self> }",
];

fn build_corpus(n: usize) -> (Codec, Vec<Vec<u8>>) {
    let samples: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("{} // {i}", TEMPLATES[i % TEMPLATES.len()]).into_bytes())
        .collect();
    let refs: Vec<&[u8]> = samples.iter().map(|v| v.as_slice()).collect();
    let codec = Codec::train(&refs).unwrap();
    let encoded: Vec<Vec<u8>> = samples.iter().map(|s| codec.compress(s)).collect();
    (codec, encoded)
}

fn bench_decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress");
    for n in [100usize, 1_000, 10_000] {
        let (codec, encoded) = build_corpus(n);
        let bytes: u64 = encoded.iter().map(|e| e.len() as u64).sum();
        group.throughput(Throughput::Bytes(bytes));
        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(codec, encoded),
            |b, (c, enc)| {
                b.iter(|| {
                    for e in enc {
                        std::hint::black_box(c.decompress(e));
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_decompress);
criterion_main!(benches);
