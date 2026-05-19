//! Baseline vs ucracc-lsp wrapper benchmark.
//!
//! Each operation is measured twice:
//!   * `*_baseline` — calling `crabcc-core` directly, no LSP layer.
//!   * `*_lsp`      — calling `Backend`'s LanguageServer trait methods.
//!
//! The delta tells us the per-call cost of the LSP wrapper (URL parsing,
//! Mutex acquire, Tokio task hop, lsp_types conversion).
//!
//! Run:
//!   cargo bench -p ucracc-lsp --bench baseline_vs_lsp

use crabcc_core::{fts::Fts, query::find_symbol, store::Store};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tower_lsp::lsp_types::{
    CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, GotoDefinitionParams, HoverParams, InitializeParams, InitializedParams,
    PartialResultParams, Position, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, Url, WorkDoneProgressParams,
    WorkspaceSymbolParams,
};
use tower_lsp::{LanguageServer, LspService};
use ucracc_lsp::server::Backend;

const RUST: &str = r#"
pub struct UcraccStore { pub name: String }
impl UcraccStore {
    pub fn open(name: &str) -> Self { Self { name: name.to_string() } }
    pub fn greet(&self) { say_hello(&self.name); }
}
fn say_hello(who: &str) { println!("hello, {who}"); }
"#;

struct Bed {
    _tmp: TempDir,
    root: PathBuf,
    store: Store,
    fts: Option<Fts>,
    rt: Arc<Runtime>,
    service: LspService<Backend>,
    rust_uri: Url,
}

fn setup() -> Bed {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::write(root.join("ucracc.rs"), RUST).unwrap();

    let db_path = root.join(".crabcc/index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let store = Store::open(&db_path).expect("open store");
    crabcc_core::index::full_index(&root, &store).expect("full_index");

    let fts_dir = root.join(".crabcc/tantivy");
    let fts = Fts::open(&fts_dir).ok();
    if let Some(f) = fts.as_ref() {
        let _ = f.rebuild(&store);
    }

    let rt = Arc::new(Runtime::new().unwrap());
    let (service, _socket) = LspService::new(Backend::new);

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
                    uri: Url::from_file_path(root.join("ucracc.rs")).unwrap(),
                    language_id: "rust".into(),
                    version: 1,
                    text: RUST.into(),
                },
            })
            .await;
    });

    let rust_uri = Url::from_file_path(root.join("ucracc.rs")).unwrap();
    Bed {
        _tmp: tmp,
        root,
        store,
        fts,
        rt,
        service,
        rust_uri,
    }
}

fn bench_cold_open(c: &mut Criterion) {
    // Cold-start surrogate: time to open the on-disk SQLite store + tantivy.
    // We pre-build them once and then re-open in the bench iter.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::write(root.join("ucracc.rs"), RUST).unwrap();
    let db = root.join(".crabcc/index.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = Store::open(&db).unwrap();
    crabcc_core::index::full_index(&root, &store).unwrap();
    let fts_dir = root.join(".crabcc/tantivy");
    let fts = Fts::open(&fts_dir).unwrap();
    let _ = fts.rebuild(&store);
    drop(store);

    c.bench_function("cold_open_baseline_store_only", |b| {
        b.iter(|| {
            let s = Store::open(black_box(&db)).unwrap();
            black_box(s);
        });
    });

    c.bench_function("cold_open_lsp_initialize_only", |b| {
        // Pure `initialize` — what the editor actually waits for before
        // it can send its first request. With lazy load, no I/O.
        let rt = Runtime::new().unwrap();
        let root = root.clone();
        b.iter(|| {
            rt.block_on(async {
                let (svc, _socket) = LspService::new(Backend::new);
                svc.inner()
                    .initialize(InitializeParams {
                        root_uri: Some(Url::from_file_path(&root).unwrap()),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                black_box(svc);
            });
        });
    });

    c.bench_function("cold_open_lsp_initialize_and_initialized", |b| {
        // Full handshake — initialize + initialized (which prefetches
        // Store + Fts in the background and waits for it). Apples to
        // the pre-lazy baseline.
        let rt = Runtime::new().unwrap();
        let root = root.clone();
        b.iter(|| {
            rt.block_on(async {
                let (svc, _socket) = LspService::new(Backend::new);
                svc.inner()
                    .initialize(InitializeParams {
                        root_uri: Some(Url::from_file_path(&root).unwrap()),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                svc.inner().initialized(InitializedParams {}).await;
                black_box(svc);
            });
        });
    });
}

fn bench_document_symbol(c: &mut Criterion) {
    let bed = setup();
    let rel = "ucracc.rs";

    c.bench_function("document_symbol_baseline", |b| {
        b.iter(|| {
            let v = bed.store.symbols_in_file(black_box(rel)).unwrap();
            black_box(v);
        });
    });

    c.bench_function("document_symbol_lsp", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .document_symbol(DocumentSymbolParams {
                        text_document: TextDocumentIdentifier {
                            uri: bed.rust_uri.clone(),
                        },
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

fn bench_definition(c: &mut Criterion) {
    let bed = setup();
    let pos = find_pos(RUST, "say_hello(&self.name)").unwrap();

    c.bench_function("definition_baseline", |b| {
        b.iter(|| {
            let v = find_symbol(&bed.store, black_box("say_hello")).unwrap();
            black_box(v);
        });
    });

    c.bench_function("definition_lsp", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .goto_definition(GotoDefinitionParams {
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier {
                                uri: bed.rust_uri.clone(),
                            },
                            position: Position {
                                line: pos.0,
                                character: pos.1,
                            },
                        },
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

fn bench_hover(c: &mut Criterion) {
    let bed = setup();
    let pos = find_pos(RUST, "pub struct UcraccStore").unwrap();

    c.bench_function("hover_baseline", |b| {
        b.iter(|| {
            let v = find_symbol(&bed.store, black_box("UcraccStore")).unwrap();
            black_box(v);
        });
    });

    c.bench_function("hover_lsp", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .hover(HoverParams {
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier {
                                uri: bed.rust_uri.clone(),
                            },
                            position: Position {
                                line: pos.0,
                                character: pos.1 + 12,
                            },
                        },
                        work_done_progress_params: WorkDoneProgressParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

fn bench_workspace_symbol(c: &mut Criterion) {
    let bed = setup();

    c.bench_function("workspace_symbol_baseline_fts_prefix", |b| {
        b.iter(|| {
            if let Some(f) = bed.fts.as_ref() {
                let v = f.prefix(black_box("Ucracc"), 20).unwrap();
                black_box(v);
            }
        });
    });

    c.bench_function("workspace_symbol_lsp", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .symbol(WorkspaceSymbolParams {
                        query: "Ucracc".into(),
                        partial_result_params: PartialResultParams::default(),
                        work_done_progress_params: WorkDoneProgressParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

fn bench_references(c: &mut Criterion) {
    let bed = setup();
    let pos = find_pos(RUST, "fn say_hello").unwrap();

    c.bench_function("references_baseline", |b| {
        b.iter(|| {
            let v = crabcc_core::query::find_refs(&bed.store, &bed.root, black_box("say_hello"))
                .unwrap();
            black_box(v);
        });
    });

    c.bench_function("references_lsp", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .references(ReferenceParams {
                        text_document_position: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier {
                                uri: bed.rust_uri.clone(),
                            },
                            position: Position {
                                line: pos.0,
                                character: pos.1,
                            },
                        },
                        context: ReferenceContext {
                            include_declaration: true,
                        },
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

fn bench_outgoing_calls(c: &mut Criterion) {
    let bed = setup();
    // Position on the `greet` identifier (the caller). outgoing_calls returns
    // what `greet` calls (say_hello).
    let pos = find_pos(RUST, "greet(&self)").unwrap();

    let item = bed.rt.block_on(async {
        let items = bed
            .service
            .inner()
            .prepare_call_hierarchy(CallHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: bed.rust_uri.clone(),
                    },
                    position: Position {
                        line: pos.0,
                        character: pos.1,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();
        items.unwrap().into_iter().next().unwrap()
    });

    // No direct baseline — outgoing_calls is built from the LSP's call-graph
    // sidecar, not via a single crabcc_core::query::* function. The LSP wrapper
    // cost is what we measure here.

    c.bench_function("outgoing_calls_lsp", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .outgoing_calls(CallHierarchyOutgoingCallsParams {
                        item: item.clone(),
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

fn find_pos(src: &str, needle: &str) -> Option<(u32, u32)> {
    for (i, l) in src.lines().enumerate() {
        if let Some(c) = l.find(needle) {
            return Some((i as u32, c as u32));
        }
    }
    None
}

// Suppress dead-code warnings on the bed.root field (it's a handy hook for
// future benches that re-walk the tree).
#[allow(dead_code)]
fn _keep(b: &Bed) -> &Path {
    &b.root
}

fn bench_cache_hit(c: &mut Criterion) {
    // Drives a definition request twice — the second hit returns from
    // the moka LRU. We measure only the warm side here; the cold side is
    // covered by `definition_lsp`.
    let bed = setup();
    let pos = find_pos(RUST, "say_hello(&self.name)").unwrap();
    // Prime the cache.
    bed.rt.block_on(async {
        let _ = bed
            .service
            .inner()
            .goto_definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: bed.rust_uri.clone(),
                    },
                    position: Position {
                        line: pos.0,
                        character: pos.1,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await;
    });

    c.bench_function("definition_lsp_cache_hit", |b| {
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .goto_definition(GotoDefinitionParams {
                        text_document_position_params: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier {
                                uri: bed.rust_uri.clone(),
                            },
                            position: Position {
                                line: pos.0,
                                character: pos.1,
                            },
                        },
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });

    c.bench_function("workspace_symbol_lsp_cache_hit", |b| {
        // Prime
        bed.rt.block_on(async {
            let _ = bed
                .service
                .inner()
                .symbol(WorkspaceSymbolParams {
                    query: "Ucracc".into(),
                    partial_result_params: PartialResultParams::default(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                })
                .await;
        });
        b.iter(|| {
            bed.rt.block_on(async {
                let resp = bed
                    .service
                    .inner()
                    .symbol(WorkspaceSymbolParams {
                        query: "Ucracc".into(),
                        partial_result_params: PartialResultParams::default(),
                        work_done_progress_params: WorkDoneProgressParams::default(),
                    })
                    .await
                    .unwrap();
                black_box(resp);
            });
        });
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(50).warm_up_time(std::time::Duration::from_millis(500));
    targets = bench_cold_open, bench_document_symbol, bench_definition, bench_hover, bench_workspace_symbol, bench_cache_hit, bench_references, bench_outgoing_calls
);
criterion_main!(benches);
