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
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, InitializeParams,
    InitializedParams, PartialResultParams, Position, ReferenceContext, ReferenceParams,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Url,
    WorkDoneProgressParams, WorkspaceSymbolParams,
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
    // Build the tantivy sidecar — workspace/symbol prefix matching needs
    // it. `crabcc index` does this automatically; doing it directly via
    // `full_index` does not. Mirror the production sequence here.
    let fts_dir = root.join(".crabcc/tantivy");
    let fts = crabcc_core::fts::Fts::open(&fts_dir).expect("open fts");
    fts.rebuild(&store).expect("fts rebuild");
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

async fn open_doc(svc: &tower_lsp::LspService<ucracc_lsp::server::Backend>, uri: Url, lang: &str, src: &str) {
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
        #[cfg(feature = "swift")]
        ("ucracc.swift", "swift", "UcraccSwift"),
    ] {
        let uri = uri_for(&root, file);
        open_doc(&svc, uri.clone(), lang_id, fixtures::all().iter().find(|(n, _)| *n == file).unwrap().1).await;

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
    let (line, character) = find_position(fixtures::RUST_SRC, "say_hello(&self.name)")
        .expect("locate call site");

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

    let (line, character) = find_position(fixtures::RUST_SRC, "pub struct UcraccStore")
        .expect("locate struct");

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
    assert!(text.contains("ucracc.rs"), "hover missing file path: {text}");
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
