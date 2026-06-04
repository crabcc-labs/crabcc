//! Concurrency / data-race stress tests for the `Backend`.
//!
//! These drive ONE shared `Backend` from many parallel Tokio tasks, with
//! reads (`hover` / `definition` / `references` / `documentSymbol` /
//! `workspaceSymbol`) interleaved against writes (`did_open` /
//! `did_change` / `did_save` / `did_close`) on overlapping URIs. The goal
//! is to shake out the shared-state hazards in the server:
//!
//!   * `open_docs` / `trees` (`DashMap`) mutated by `did_change` while
//!     readers hold per-key refs (`word_at`, reparse).
//!   * the moka `cache` being `invalidate_all`'d under concurrent lookups.
//!   * the `store` / `fts` / `graph` (`std::sync::Mutex<Option<…>>`)
//!     re-entered from `spawn_blocking` re-index tasks.
//!
//! What we assert is deliberately black-box: **no panic, no deadlock**
//! (every test is wrapped in a wall-clock timeout — a hang fails loudly
//! instead of stalling CI) and **final-state consistency** (after the
//! last write wins, a read reflects it). For true *data-race* (UB)
//! detection compile these under ThreadSanitizer — see
//! `scripts/lsp-race-tsan.sh` / `task lsp-race`.
//!
//! Run: cargo test -p ucracc-lsp --test concurrency

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    HoverParams, InitializeParams, InitializedParams, PartialResultParams, Position,
    ReferenceContext, ReferenceParams, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams, WorkspaceSymbolParams,
};
use tower_lsp::{LanguageServer, LspService};
use ucracc_lsp::server::Backend;

/// A thread-shareable handle to the live `Backend`.
///
/// `LspService<Backend>` is `!Sync` *solely* because of its client→server
/// message socket and pending-response map. The `Backend` it wraps is
/// `Send + Sync` — the `LanguageServer` trait bounds require it, and every
/// field is an `Arc<DashMap>` / `Arc<Mutex<…>>` / `Arc<…>` / a `Clone`able
/// `Client`. These tests reach the server **only** through [`Shared::be`]
/// (`-> &Backend`); the `!Sync` socket internals are never reachable here.
struct Shared(LspService<Backend>);

// SAFETY: upheld by the `assert_send_sync::<Backend>()` below — `Backend` is
// `Send + Sync`, and `Shared` exposes nothing but `&Backend`. The only
// `!Sync` parts of `LspService` (its socket / pending map) are unreachable
// through this wrapper, so concurrent `&Shared` access is data-race-free.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    // If `Backend` ever loses `Send + Sync`, this stops compiling and the
    // `unsafe impl`s above must be re-justified.
    assert_send_sync::<Backend>();
};

impl Shared {
    #[inline]
    fn be(&self) -> &Backend {
        self.0.inner()
    }
}

/// Two valid Rust revisions of the same document — `did_change`
/// full-replace flips between them. Both define `UcraccConc` so symbol
/// lookups stay meaningful across edits; the function bodies differ so the
/// reparse actually does work.
const REV_A: &str = r#"
pub struct UcraccConc { pub n: u64 }
impl UcraccConc {
    pub fn make(n: u64) -> Self { Self { n } }
    pub fn step(&self) -> u64 { helper_a(self.n) }
}
fn helper_a(x: u64) -> u64 { x.wrapping_add(1) }
"#;

const REV_B: &str = r#"
pub struct UcraccConc { pub n: u64, pub tag: u8 }
impl UcraccConc {
    pub fn make(n: u64) -> Self { Self { n, tag: 0 } }
    pub fn step(&self) -> u64 { helper_b(self.n).wrapping_mul(2) }
}
fn helper_b(x: u64) -> u64 { x.wrapping_sub(1) }
fn extra_b() {}
"#;

/// Build an on-disk index so definition/references/workspaceSymbol have
/// real rows to return. Fuzzy/prefix read the live SQLite index, so a plain
/// `full_index` is the whole setup.
fn build_index(root: &Path, primary: &str) {
    std::fs::write(root.join("conc.rs"), primary).expect("write fixture");
    let db_path = root.join(".crabcc/index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let store = crabcc_core::store::Store::open(&db_path).expect("open store");
    crabcc_core::index::full_index(root, &store).expect("full_index");
}

async fn boot(root: &Path) -> Arc<Shared> {
    let (service, _socket) = LspService::new(Backend::new);
    service
        .inner()
        .initialize(InitializeParams {
            root_uri: Some(Url::from_file_path(root).unwrap()),
            ..Default::default()
        })
        .await
        .expect("initialize");
    service.inner().initialized(InitializedParams {}).await;
    Arc::new(Shared(service))
}

fn uri(root: &Path, name: &str) -> Url {
    Url::from_file_path(root.join(name)).expect("file uri")
}

// ---- param builders (mirror the tower-lsp JSON-RPC decode shapes) -------

fn open_params(uri: Url, text: &str) -> DidOpenTextDocumentParams {
    DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri,
            language_id: "rust".into(),
            version: 1,
            text: text.into(),
        },
    }
}

/// Full-replace `did_change` (`range: None`) — the simplest write that
/// still drives `open_docs` insert + `cache.invalidate_all` + `trees`
/// drop + re-index.
fn change_params(uri: Url, version: i32, text: &str) -> DidChangeTextDocumentParams {
    DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier { uri, version },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.into(),
        }],
    }
}

fn pos_params(uri: Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri },
        position: Position { line, character },
    }
}

fn hover_params(uri: Url) -> HoverParams {
    HoverParams {
        // line 1 / col ~12 lands inside `UcraccConc` in REV_A/REV_B.
        text_document_position_params: pos_params(uri, 1, 12),
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

fn def_params(uri: Url) -> GotoDefinitionParams {
    GotoDefinitionParams {
        text_document_position_params: pos_params(uri, 1, 12),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn ref_params(uri: Url) -> ReferenceParams {
    ReferenceParams {
        text_document_position: pos_params(uri, 1, 12),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    }
}

fn doc_sym_params(uri: Url) -> DocumentSymbolParams {
    DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn ws_sym_params() -> WorkspaceSymbolParams {
    WorkspaceSymbolParams {
        query: "Ucracc".into(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

/// Fail loudly on a hang instead of letting the test runner stall: every
/// scenario runs inside this wall-clock budget.
async fn within<F: std::future::Future>(secs: u64, label: &str, fut: F) -> F::Output {
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(v) => v,
        Err(_) => panic!("`{label}` did not finish within {secs}s — suspected deadlock/hang"),
    }
}

/// All reader/writer surfaces hammered together on a shared set of URIs.
/// Deterministic op selection (no `rand` dependency) so a failure
/// reproduces. Passes iff nothing panics and the whole fan-out drains
/// inside the timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_ops_under_concurrency_no_deadlock() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_index(&root, REV_A);
    let svc = boot(&root).await;

    // A small pool of overlapping URIs: the indexed file plus a couple of
    // scratch docs that only ever live in `open_docs` / `trees`.
    let uris: Vec<Url> = ["conc.rs", "scratch_a.rs", "scratch_b.rs"]
        .iter()
        .map(|n| uri(&root, n))
        .collect();

    const TASKS: usize = 12;
    const ITERS: i32 = 40;

    let mut handles = Vec::with_capacity(TASKS);
    for t in 0..TASKS {
        let svc = svc.clone();
        let uris = uris.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..ITERS {
                let u = uris[(t + i as usize) % uris.len()].clone();
                // 9-way deterministic interleave across the full surface.
                match (t * 7 + i as usize * 3) % 9 {
                    0 => svc.be().did_open(open_params(u, REV_A)).await,
                    1 => {
                        let rev = if i % 2 == 0 { REV_A } else { REV_B };
                        svc.be().did_change(change_params(u, i + 2, rev)).await
                    }
                    2 => {
                        let _ = svc.be().hover(hover_params(u)).await;
                    }
                    3 => {
                        let _ = svc.be().goto_definition(def_params(u)).await;
                    }
                    4 => {
                        let _ = svc.be().references(ref_params(u)).await;
                    }
                    5 => {
                        let _ = svc.be().document_symbol(doc_sym_params(u)).await;
                    }
                    6 => {
                        let _ = svc.be().symbol(ws_sym_params()).await;
                    }
                    7 => {
                        svc.be()
                            .did_save(DidSaveTextDocumentParams {
                                text_document: TextDocumentIdentifier { uri: u },
                                text: None,
                            })
                            .await
                    }
                    _ => {
                        svc.be()
                            .did_close(DidCloseTextDocumentParams {
                                text_document: TextDocumentIdentifier { uri: u },
                            })
                            .await
                    }
                }
            }
        }));
    }

    within(60, "mixed_ops fan-out", async {
        for h in handles {
            h.await.expect("worker task panicked");
        }
    })
    .await;
}

/// Many writers flipping a single document between two revisions while
/// readers query it concurrently — targets the `open_docs`/`trees`/cache
/// interplay on one key. After the dust settles, a final known write must
/// be observable through `documentSymbol` (last-write-wins consistency).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writers_and_readers_same_doc_stay_consistent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_index(&root, REV_A);
    let svc = boot(&root).await;
    let u = uri(&root, "conc.rs");

    svc.be().did_open(open_params(u.clone(), REV_A)).await;

    const WRITERS: usize = 6;
    const READERS: usize = 6;
    const ITERS: i32 = 50;

    let mut handles = Vec::new();
    for w in 0..WRITERS {
        let svc = svc.clone();
        let u = u.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..ITERS {
                let rev = if (w + i as usize) % 2 == 0 {
                    REV_A
                } else {
                    REV_B
                };
                svc.be()
                    .did_change(change_params(u.clone(), i + 2, rev))
                    .await;
            }
        }));
    }
    for _ in 0..READERS {
        let svc = svc.clone();
        let u = u.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..ITERS {
                let _ = svc.be().document_symbol(doc_sym_params(u.clone())).await;
                let _ = svc.be().hover(hover_params(u.clone())).await;
            }
        }));
    }

    within(60, "writers/readers fan-out", async {
        for h in handles {
            h.await.expect("worker task panicked");
        }
    })
    .await;

    // Last write wins: drive a final known revision, then the in-memory
    // mirror a reader sees must reflect exactly it.
    svc.be()
        .did_change(change_params(u.clone(), 10_000, REV_B))
        .await;
    let resp = svc
        .be()
        .document_symbol(doc_sym_params(u.clone()))
        .await
        .expect("documentSymbol ok");
    let names: Vec<String> = match resp {
        Some(DocumentSymbolResponse::Nested(s)) => s.into_iter().map(|d| d.name).collect(),
        Some(DocumentSymbolResponse::Flat(s)) => s.into_iter().map(|d| d.name).collect(),
        None => Vec::new(),
    };
    // `extra_b` exists only in REV_B — its presence proves the final
    // replace is the state readers observe (no torn/stale mirror).
    assert!(
        names.iter().any(|n| n == "extra_b"),
        "final REV_B not reflected in documentSymbol; got {names:?}"
    );
}

/// Rapid open/close churn on a key concurrent with reads — exercises
/// `trees.remove` / `open_docs.remove` racing `word_at` + reparse lookups.
/// A read landing between a close and the next open must degrade to an
/// empty result, never panic or wedge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_close_churn_with_concurrent_reads() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_index(&root, REV_A);
    let svc = boot(&root).await;
    let u = uri(&root, "conc.rs");

    const TASKS: usize = 8;
    const ITERS: i32 = 60;

    let mut handles = Vec::new();
    for t in 0..TASKS {
        let svc = svc.clone();
        let u = u.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..ITERS {
                if t % 2 == 0 {
                    // churners
                    if i % 2 == 0 {
                        svc.be().did_open(open_params(u.clone(), REV_A)).await;
                    } else {
                        svc.be()
                            .did_close(DidCloseTextDocumentParams {
                                text_document: TextDocumentIdentifier { uri: u.clone() },
                            })
                            .await;
                    }
                } else {
                    // readers
                    let _ = svc.be().hover(hover_params(u.clone())).await;
                    let _ = svc.be().document_symbol(doc_sym_params(u.clone())).await;
                }
            }
        }));
    }

    within(60, "open/close churn", async {
        for h in handles {
            h.await.expect("worker task panicked");
        }
    })
    .await;
}
