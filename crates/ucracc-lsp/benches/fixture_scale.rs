//! Scaling benchmark: how the LSP's hot paths and write path behave as the
//! indexed document grows. Where `baseline_vs_lsp` isolates the *per-call*
//! wrapper overhead on a tiny fixture, this one parameterizes the **fixture
//! size** (symbol count) so regressions that only show up at scale —
//! superlinear query cost, an O(n²) reindex — are visible.
//!
//! Surfaces measured per size:
//!   * `document_symbol` — read path, output grows with the file.
//!   * `workspace_symbol` — tantivy prefix query across the whole index.
//!   * `references`       — call-graph fan-in (many callers of one fn).
//!   * `hover`            — point lookup (should stay ~flat vs size).
//!   * `did_change`       — write path: full reparse + store replace.
//!
//! Run:
//!   cargo bench -p ucracc-lsp --bench fixture_scale
//!   # quick compile+smoke (one iteration, no measurement):
//!   cargo bench -p ucracc-lsp --bench fixture_scale -- --test

use crabcc_core::{fts::Fts, store::Store};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentSymbolParams,
    GotoDefinitionParams, HoverParams, InitializeParams, InitializedParams, PartialResultParams,
    Position, ReferenceContext, ReferenceParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Url,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceSymbolParams,
};
use tower_lsp::{LanguageServer, LspService};
use ucracc_lsp::server::Backend;

/// Generate a self-contained Rust source with `n` functions plus a struct
/// and a shared leaf, wired so the call graph has real fan-in: every
/// even-indexed function calls the common `ucracc_leaf` (giving `references`
/// something ~n/2 wide to resolve), while odd ones chain to their
/// predecessor.
fn gen_rust(n: usize) -> String {
    let mut s = String::with_capacity(n * 56 + 128);
    s.push_str("pub struct UcraccScale { pub items: Vec<u64> }\n");
    s.push_str("impl UcraccScale {\n");
    s.push_str("    pub fn new() -> Self { Self { items: Vec::new() } }\n");
    s.push_str("    pub fn total(&self) -> u64 { self.items.iter().copied().sum() }\n");
    s.push_str("}\n");
    s.push_str("fn ucracc_leaf(x: u64) -> u64 { x.wrapping_add(1) }\n");
    for i in 0..n {
        if i % 2 == 0 || i == 0 {
            s.push_str(&format!(
                "fn ucracc_fn_{i}(x: u64) -> u64 {{ ucracc_leaf(x).wrapping_mul({m}) }}\n",
                m = (i % 7) + 1
            ));
        } else {
            s.push_str(&format!(
                "fn ucracc_fn_{i}(x: u64) -> u64 {{ ucracc_fn_{p}(x).wrapping_add(1) }}\n",
                p = i - 1
            ));
        }
    }
    s
}

struct Bed {
    _tmp: TempDir,
    rt: Arc<Runtime>,
    service: LspService<Backend>,
    uri: Url,
    text: String,
}

fn setup(n: usize) -> Bed {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let text = gen_rust(n);
    std::fs::write(root.join("scale.rs"), &text).unwrap();

    let db_path = root.join(".crabcc/index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let store = Store::open(&db_path).expect("open store");
    crabcc_core::index::full_index(&root, &store).expect("full_index");
    let fts = Fts::open(&root.join(".crabcc/tantivy")).expect("open fts");
    fts.rebuild(&store).expect("fts rebuild");

    let rt = Arc::new(Runtime::new().unwrap());
    let (service, _socket) = LspService::new(Backend::new);
    let uri = Url::from_file_path(root.join("scale.rs")).unwrap();
    rt.block_on(async {
        service
            .inner()
            .initialize(InitializeParams {
                root_uri: Some(Url::from_file_path(&root).unwrap()),
                ..Default::default()
            })
            .await
            .unwrap();
        service.inner().initialized(InitializedParams {}).await;
        service
            .inner()
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "rust".into(),
                    version: 1,
                    text: text.clone(),
                },
            })
            .await;
    });

    Bed {
        _tmp: tmp,
        rt,
        service,
        uri,
        text,
    }
}

fn pos(uri: Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri },
        position: Position { line, character },
    }
}

fn bench_scaling(c: &mut Criterion) {
    // Keep sizes modest so the suite stays CI-friendly; the trend across
    // them is what matters, not absolute counts.
    const SIZES: [usize; 3] = [64, 256, 1024];

    let mut group = c.benchmark_group("ucracc_lsp_scale");
    // Long enough to be stable, short enough to finish; reindex at 1k symbols
    // dominates the wall time.
    group.sample_size(30);

    for &n in &SIZES {
        let bed = setup(n);
        let be = bed.service.inner();
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("document_symbol", n), &n, |b, _| {
            b.iter(|| {
                bed.rt.block_on(async {
                    black_box(
                        be.document_symbol(DocumentSymbolParams {
                            text_document: TextDocumentIdentifier {
                                uri: bed.uri.clone(),
                            },
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                        })
                        .await
                        .unwrap(),
                    )
                })
            })
        });

        group.bench_with_input(BenchmarkId::new("workspace_symbol", n), &n, |b, _| {
            b.iter(|| {
                bed.rt.block_on(async {
                    black_box(
                        be.symbol(WorkspaceSymbolParams {
                            query: "ucracc".into(),
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                        })
                        .await
                        .unwrap(),
                    )
                })
            })
        });

        group.bench_with_input(BenchmarkId::new("references", n), &n, |b, _| {
            // `ucracc_leaf` is declared on line 5 (0-based); col 3 lands in
            // its name. It's called by ~n/2 functions.
            b.iter(|| {
                bed.rt.block_on(async {
                    black_box(
                        be.references(ReferenceParams {
                            text_document_position: pos(bed.uri.clone(), 5, 3),
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                            context: ReferenceContext {
                                include_declaration: true,
                            },
                        })
                        .await
                        .unwrap(),
                    )
                })
            })
        });

        group.bench_with_input(BenchmarkId::new("hover", n), &n, |b, _| {
            // Point lookup on `UcraccScale` (line 0) — expected ~flat vs size.
            b.iter(|| {
                bed.rt.block_on(async {
                    black_box(
                        be.hover(HoverParams {
                            text_document_position_params: pos(bed.uri.clone(), 0, 12),
                            work_done_progress_params: WorkDoneProgressParams::default(),
                        })
                        .await
                        .unwrap(),
                    )
                })
            })
        });

        // Keep `goto_definition` honest too — it shares the resolver with
        // hover but returns a Location.
        group.bench_with_input(BenchmarkId::new("goto_definition", n), &n, |b, _| {
            b.iter(|| {
                bed.rt.block_on(async {
                    black_box(
                        be.goto_definition(GotoDefinitionParams {
                            text_document_position_params: pos(bed.uri.clone(), 0, 12),
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                        })
                        .await
                        .unwrap(),
                    )
                })
            })
        });

        // Write path: full-replace `did_change` → reparse + store replace.
        let mut ver = 2;
        group.bench_with_input(BenchmarkId::new("did_change_reindex", n), &n, |b, _| {
            b.iter(|| {
                bed.rt.block_on(async {
                    ver += 1;
                    be.did_change(DidChangeTextDocumentParams {
                        text_document: VersionedTextDocumentIdentifier {
                            uri: bed.uri.clone(),
                            version: ver,
                        },
                        content_changes: vec![TextDocumentContentChangeEvent {
                            range: None,
                            range_length: None,
                            text: bed.text.clone(),
                        }],
                    })
                    .await
                })
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_scaling);
criterion_main!(benches);
