//! In-process end-to-end test. Drives the real `Backend` through its
//! `LanguageServer` trait methods, exactly as tower-lsp would when
//! decoding JSON-RPC messages off the wire. We skip the JSON-RPC layer
//! because that's tower-lsp's responsibility, not ours.
//!
//! Run: cargo test -p ucracc-lsp --test integration_lsp

mod fixtures;

use crabcc_core::store::Store;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyPrepareParams, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, HoverParams, InitializeParams, InitializedParams, PartialResultParams,
    Position, ReferenceContext, ReferenceParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Url,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceSymbolParams,
};
use tower_lsp::{LanguageServer, LspService};

fn write_fixtures(root: &Path) {
    for (name, src) in fixtures::all() {
        std::fs::write(root.join(name), src).expect("write fixture");
    }
}

fn build_index(root: &Path) {
    let db_path = root.join(".crabcc/index.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let store = Store::open(&db_path).expect("open store");
    crabcc_core::index::full_index(root, &store).expect("full_index");
    // Fuzzy/prefix (workspace/symbol) now read the live SQLite index — no
    // sidecar to build, so `full_index` is all the setup needed.
}

fn uri_for(root: &Path, name: &str) -> Url {
    Url::from_file_path(root.join(name)).expect("file uri")
}

async fn boot(root: PathBuf) -> tower_lsp::LspService<ucracc_lsp::server::Backend> {
    let (service, _socket) = LspService::new(ucracc_lsp::server::Backend::new);
    let backend = service.inner();
    let init = InitializeParams {
        root_uri: Some(Url::from_file_path(&root).unwrap()),
        ..Default::default()
    };
    backend.initialize(init).await.expect("initialize");
    backend.initialized(InitializedParams {}).await;
    service
}

async fn open_doc(
    svc: &tower_lsp::LspService<ucracc_lsp::server::Backend>,
    uri: Url,
    lang: &str,
    src: &str,
) {
    svc.inner()
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: lang.to_string(),
                version: 1,
                text: src.to_string(),
            },
        })
        .await;
}

/// A document with multiple top-level symbols round-trips through
/// `documentSymbol` for every supported language.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn document_symbol_covers_all_languages() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;

    for (file, lang_id, expected_name) in [
        ("ucracc.rs", "rust", "UcraccStore"),
        ("ucracc.ts", "typescript", "UcraccClient"),
        ("ucracc.py", "python", "UcraccPipeline"),
        ("ucracc.rb", "ruby", "UcraccRuby"),
        ("ucracc.go", "go", "UcraccGo"),
        // Swift and Bash moved to crabcc-core as of v0.2.0 — they're always
        // available now, no feature gate required.
        ("ucracc.swift", "swift", "UcraccSwift"),
        ("ucracc.sh", "shellscript", "ucracc_greet"),
        #[cfg(feature = "yaml")]
        ("ucracc.yaml", "yaml", "jobs"),
        #[cfg(feature = "markdown")]
        ("ucracc.md", "markdown", "UcraccLsp"),
    ] {
        let uri = uri_for(&root, file);
        open_doc(
            &svc,
            uri.clone(),
            lang_id,
            fixtures::all().iter().find(|(n, _)| *n == file).unwrap().1,
        )
        .await;

        let resp = svc
            .inner()
            .document_symbol(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("documentSymbol");

        let symbols = match resp {
            Some(DocumentSymbolResponse::Nested(s)) => s,
            other => panic!("expected nested DocumentSymbol for {lang_id}, got {other:?}"),
        };
        assert!(
            symbols.iter().any(|s| s.name == expected_name),
            "{} missing in documentSymbol for {lang_id}: got {:?}",
            expected_name,
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }
}

/// `goto_definition` for `say_hello` from inside the Rust fixture must
/// land on the free fn definition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goto_definition_rust() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    // Position chosen to land somewhere inside `say_hello` on the
    // `greet` body. We find the line and column manually for stability.
    let (line, character) =
        find_position(fixtures::RUST_SRC, "say_hello(&self.name)").expect("locate call site");

    let resp = svc
        .inner()
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("definition");

    let locs = match resp.expect("Some") {
        GotoDefinitionResponse::Array(v) => v,
        GotoDefinitionResponse::Scalar(l) => vec![l],
        other => panic!("unexpected definition response: {other:?}"),
    };
    assert!(!locs.is_empty(), "no definition found");
    assert!(
        locs.iter().any(|l| l.uri.path().ends_with("ucracc.rs")),
        "definition not in ucracc.rs: {locs:?}"
    );
}

/// `references` for `say_hello` must include the call site inside `greet`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn references_returns_call_site() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    let (line, character) =
        find_position(fixtures::RUST_SRC, "fn say_hello(who").expect("locate def");

    let resp = svc
        .inner()
        .references(ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line,
                    character: character + 3, // somewhere inside the identifier
                },
            },
            context: ReferenceContext {
                include_declaration: true,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("references");

    let locs = resp.expect("Some");
    assert!(!locs.is_empty(), "no references found");
    assert!(locs.iter().any(|l| l.uri.path().ends_with("ucracc.rs")));
}

/// `hover` on a known symbol must produce markdown that contains the
/// symbol name and its file path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hover_returns_signature() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    let (line, character) =
        find_position(fixtures::RUST_SRC, "pub struct UcraccStore").expect("locate struct");

    let resp = svc
        .inner()
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line,
                    character: character + 12, // inside `UcraccStore`
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover");

    let hover = resp.expect("hover Some");
    let text = match hover.contents {
        tower_lsp::lsp_types::HoverContents::Markup(m) => m.value,
        other => panic!("unexpected hover contents: {other:?}"),
    };
    assert!(text.contains("UcraccStore"), "hover missing symbol: {text}");
    assert!(
        text.contains("ucracc.rs"),
        "hover missing file path: {text}"
    );
}

/// `workspace/symbol` with a prefix must return matches across files.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_prefix_match() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;

    let resp = svc
        .inner()
        .symbol(WorkspaceSymbolParams {
            query: "Ucracc".to_string(),
            partial_result_params: PartialResultParams::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("workspace/symbol");

    let syms = resp.expect("Some");
    assert!(
        syms.iter().any(|s| s.name.starts_with("Ucracc")),
        "no Ucracc-prefixed symbol returned: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// (line, character) of the first byte of `needle` in `src`. Both are
/// 0-based, as LSP positions are.
fn find_position(src: &str, needle: &str) -> Option<(u32, u32)> {
    for (i, line) in src.lines().enumerate() {
        if let Some(col) = line.find(needle) {
            return Some((i as u32, col as u32));
        }
    }
    None
}

/// `references` for `say_hello` must include hits from BOTH the defining
/// file (`ucracc.rs`) and the user file (`ucracc_user.rs`). This is the
/// real-world ask: rename support / call-site auditing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn references_finds_cross_file_hits() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    // Add the second Rust file that calls say_hello.
    std::fs::write(root.join("ucracc_user.rs"), fixtures::RUST_USER_SRC).unwrap();
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    let (line, character) =
        find_position(fixtures::RUST_SRC, "fn say_hello(who").expect("locate def");

    let resp = svc
        .inner()
        .references(ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line,
                    character: character + 3,
                },
            },
            context: ReferenceContext {
                include_declaration: true,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("references");

    let locs = resp.expect("Some");
    let files: std::collections::HashSet<String> = locs
        .iter()
        .map(|l| {
            l.uri
                .to_file_path()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert!(
        files.contains("ucracc.rs") && files.contains("ucracc_user.rs"),
        "expected hits in both ucracc.rs and ucracc_user.rs; got files: {files:?}, locs: {locs:?}"
    );
}

/// The cache must NOT serve stale results across an edit. We:
///   1. open the file and prime the cache via a hover request,
///   2. send didChange that renames `UcraccStore` -> `UcraccStoreV2`,
///   3. ask for hover on `UcraccStoreV2`,
///   4. assert it resolves.
/// If the cache weren't flushed on didChange, step 4 would fail because
/// the old store has no `UcraccStoreV2` symbol.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_invalidates_on_did_change() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    // Prime the cache.
    let (l, c) = find_position(fixtures::RUST_SRC, "pub struct UcraccStore").unwrap();
    let _ = svc
        .inner()
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line: l,
                    character: c + 12,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .unwrap();

    // Edit: rename UcraccStore -> UcraccStoreV2 across the whole file.
    let renamed = fixtures::RUST_SRC.replace("UcraccStore", "UcraccStoreV2");
    svc.inner()
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: renamed.clone(),
            }],
        })
        .await;

    // After the edit, hover on the new name MUST find it.
    let (l, c) = find_position(&renamed, "pub struct UcraccStoreV2").unwrap();
    let resp = svc
        .inner()
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: l,
                    character: c + 12,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover after edit");
    let hover = resp.expect("hover Some after edit");
    let text = match hover.contents {
        tower_lsp::lsp_types::HoverContents::Markup(m) => m.value,
        other => panic!("unexpected contents: {other:?}"),
    };
    assert!(
        text.contains("UcraccStoreV2"),
        "stale cache served old symbol; hover text: {text}"
    );
}

/// `workspace/symbol` relies on a cached fuzzy/prefix snapshot of the symbol
/// table. That snapshot MUST be dropped on edits, or prefix search keeps
/// serving the pre-edit name list. (Regression guard for the P2 raised on the
/// Tantivy-removal PR: the snapshot was never invalidated.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_symbol_reflects_did_change() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    async fn has_symbol(
        svc: &LspService<ucracc_lsp::server::Backend>,
        query: &str,
    ) -> bool {
        svc.inner()
            .symbol(WorkspaceSymbolParams {
                query: query.to_string(),
                partial_result_params: PartialResultParams::default(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .expect("workspace/symbol")
            .map(|syms| syms.iter().any(|s| s.name == query))
            .unwrap_or(false)
    }

    // Prime the snapshot and confirm the post-rename name is absent first.
    assert!(
        has_symbol(&svc, "UcraccStore").await,
        "baseline UcraccStore not found"
    );
    assert!(
        !has_symbol(&svc, "UcraccStoreV2").await,
        "UcraccStoreV2 present before the edit?!"
    );

    // Rename UcraccStore -> UcraccStoreV2 across the whole file.
    let renamed = fixtures::RUST_SRC.replace("UcraccStore", "UcraccStoreV2");
    svc.inner()
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: renamed.clone(),
            }],
        })
        .await;

    // The snapshot must have been invalidated, so the new name is searchable.
    assert!(
        has_symbol(&svc, "UcraccStoreV2").await,
        "workspace/symbol served a stale fts snapshot after did_change"
    );
}

/// Incremental reparse path: send a small-range `didChange` (not a full
/// replace), then assert the LSP re-indexes correctly. Catches regressions
/// where the cached `Tree` is reused without applying the InputEdit, or
/// where the byte-offset math drifts off the actual source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_reparse_picks_up_small_edit() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    // Find `UcraccStore` and tack an `X` onto the end, simulating a typed
    // character. This is a ranged didChange — the range is the empty
    // span just AFTER the `e` in `UcraccStore`.
    let (line, col_of_pub) = find_position(fixtures::RUST_SRC, "pub struct UcraccStore").unwrap();
    let end_col = col_of_pub + ("pub struct UcraccStore".len() as u32);
    svc.inner()
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(tower_lsp::lsp_types::Range {
                    start: Position {
                        line,
                        character: end_col,
                    },
                    end: Position {
                        line,
                        character: end_col,
                    },
                }),
                range_length: None,
                text: "X".to_string(),
            }],
        })
        .await;

    // The original UcraccStore should have been renamed to UcraccStoreX
    // in the live index. Hover on UcraccStoreX must resolve.
    let resp = svc
        .inner()
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line,
                    character: col_of_pub + 12,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover")
        .expect("Some(hover)");
    let text = match resp.contents {
        tower_lsp::lsp_types::HoverContents::Markup(m) => m.value,
        other => panic!("unexpected hover contents: {other:?}"),
    };
    assert!(
        text.contains("UcraccStoreX"),
        "incremental reparse missed the edit; hover text: {text}"
    );
}

/// `callHierarchy/prepare` + `callHierarchy/incomingCalls` must surface
/// `greet` as a caller of `say_hello` in the Rust fixture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_hierarchy_incoming_from_call_edge() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);
    build_index(&root);

    let svc = boot(root.clone()).await;
    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&svc, uri.clone(), "rust", fixtures::RUST_SRC).await;

    let (l, c) = find_position(fixtures::RUST_SRC, "fn say_hello(who").unwrap();
    let prepared = svc
        .inner()
        .prepare_call_hierarchy(CallHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line: l,
                    character: c + 3,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("prepare");
    let item = prepared
        .expect("Some(items)")
        .into_iter()
        .next()
        .expect("at least one hierarchy item");
    assert_eq!(item.name, "say_hello");

    let incoming = svc
        .inner()
        .incoming_calls(CallHierarchyIncomingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("incoming");
    let calls = incoming.expect("Some(calls)");
    assert!(
        !calls.is_empty(),
        "expected at least one incoming caller of say_hello, got none"
    );
}

/// A client (e.g. the Zed extension forwarding
/// `lsp.ucracc-lsp.initialization_options`) can point the server at a
/// `.crabcc` directory that is NOT the default `<root>/.crabcc`. We build
/// the index *only* at a custom location and assert `documentSymbol`
/// still resolves — proving `indexPath` is honored and isn't silently
/// falling back to the default path (which doesn't exist here).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialization_options_index_path_override() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_fixtures(&root);

    // Build the index under `<root>/out/.crabcc`, leaving `<root>/.crabcc`
    // deliberately absent.
    let crabcc_dir = root.join("out/.crabcc");
    let db_path = crabcc_dir.join("index.db");
    std::fs::create_dir_all(&crabcc_dir).unwrap();
    let store = Store::open(&db_path).expect("open store");
    crabcc_core::index::full_index(&root, &store).expect("full_index");
    assert!(
        !root.join(".crabcc").exists(),
        "default path must be absent"
    );

    // Boot with an explicit relative `indexPath`.
    let (service, _socket) = LspService::new(ucracc_lsp::server::Backend::new);
    let backend = service.inner();
    backend
        .initialize(InitializeParams {
            root_uri: Some(Url::from_file_path(&root).unwrap()),
            initialization_options: Some(serde_json::json!({ "indexPath": "out/.crabcc" })),
            ..Default::default()
        })
        .await
        .expect("initialize");
    backend.initialized(InitializedParams {}).await;

    let uri = uri_for(&root, "ucracc.rs");
    open_doc(&service, uri.clone(), "rust", fixtures::RUST_SRC).await;

    let resp = service
        .inner()
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("document_symbol");
    let syms = match resp.expect("Some(response)") {
        DocumentSymbolResponse::Nested(s) => s,
        DocumentSymbolResponse::Flat(_) => panic!("expected nested document symbols"),
    };
    assert!(
        !syms.is_empty(),
        "expected symbols resolved via custom indexPath, got none"
    );
}
