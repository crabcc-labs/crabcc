use crate::cache::{Key as CacheKey, LruCache};
use crate::commands;
use crate::handlers;
use crate::lang::{Lang, SUPPORTED_LANGUAGE_IDS};
use std::sync::Arc as StdArc;
use anyhow::Result as AResult;
use crabcc_core::{
    fts::Fts,
    graph::CallGraph,
    hash, index,
    query::{self, find_callers, find_symbol},
    store::Store,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};
use tracing::{info, warn};

pub struct Backend {
    pub client: Client,
    pub state: Arc<RwLock<State>>,
}

pub struct State {
    pub repo_root: PathBuf,
    /// SQLite store. Behind a sync Mutex because rusqlite::Connection is
    /// !Sync; we never hold it across `.await`.
    pub store: std::sync::Mutex<Option<Store>>,
    pub fts: std::sync::Mutex<Option<Fts>>,
    pub graph: std::sync::Mutex<Option<CallGraph>>,
    /// In-memory mirror of open documents; LSP gives us deltas but the
    /// indexer needs the full source.
    pub open_docs: HashMap<Url, String>,
    /// Read-through LRU for repeated identical queries. Flushed on every
    /// write event.
    pub cache: LruCache,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(State {
                repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                store: std::sync::Mutex::new(None),
                fts: std::sync::Mutex::new(None),
                graph: std::sync::Mutex::new(None),
                open_docs: HashMap::new(),
                cache: LruCache::new(),
            })),
        }
    }

    fn server_capabilities() -> ServerCapabilities {
        ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::FULL,
            )),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            document_symbol_provider: Some(OneOf::Left(true)),
            workspace_symbol_provider: Some(OneOf::Left(true)),
            call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
            execute_command_provider: {
                let cmds = commands::known_commands();
                if cmds.is_empty() {
                    None
                } else {
                    Some(ExecuteCommandOptions {
                        commands: cmds,
                        work_done_progress_options: Default::default(),
                    })
                }
            },
            ..Default::default()
        }
    }
}

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        let root: PathBuf = params
            .root_uri
            .as_ref()
            .and_then(|u| u.to_file_path().ok())
            .or_else(|| params.workspace_folders.as_ref().and_then(|wf| {
                wf.first().and_then(|f| f.uri.to_file_path().ok())
            }))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let db_path = root.join(".crabcc/index.db");
        let fts_dir = root.join(".crabcc/tantivy");

        let (store_opt, fts_opt) = tokio::task::spawn_blocking({
            let db = db_path.clone();
            let fts = fts_dir.clone();
            move || {
                let s = if db.exists() { Store::open(&db).ok() } else { None };
                let f = if fts.exists() { Fts::open(&fts).ok() } else { None };
                (s, f)
            }
        })
        .await
        .unwrap_or((None, None));

        let mut st = self.state.write().await;
        st.repo_root = root.clone();
        *st.store.lock().unwrap() = store_opt;
        *st.fts.lock().unwrap() = fts_opt;
        drop(st);

        info!(
            target: "ucracc_lsp",
            root = %root.display(),
            langs = SUPPORTED_LANGUAGE_IDS.len(),
            "initialized"
        );

        Ok(InitializeResult {
            capabilities: Self::server_capabilities(),
            server_info: Some(ServerInfo {
                name: "ucracc-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let st = self.state.read().await;
        let has_store = st.store.lock().unwrap().is_some();
        if !has_store {
            warn!(target: "ucracc_lsp", "no .crabcc/index.db at {} — run `crabcc index` first", st.repo_root.display());
            let _ = self
                .client
                .show_message(
                    MessageType::WARNING,
                    "ucracc-lsp: no .crabcc/index.db found; run `crabcc index` to enable navigation.",
                )
                .await;
        }
    }

    async fn shutdown(&self) -> RpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let mut st = self.state.write().await;
        st.open_docs.insert(p.text_document.uri.clone(), p.text_document.text.clone());
        let root = st.repo_root.clone();
        st.cache.invalidate_all();
        drop(st);
        self.index_uri(&p.text_document.uri, &p.text_document.text, &root).await;
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        // FULL sync — server capabilities request FULL, so each event is the whole doc.
        if let Some(change) = p.content_changes.into_iter().next() {
            let mut st = self.state.write().await;
            st.open_docs.insert(p.text_document.uri.clone(), change.text.clone());
            let root = st.repo_root.clone();
            st.cache.invalidate_all();
            drop(st);
            self.index_uri(&p.text_document.uri, &change.text, &root).await;
        }
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        self.state.read().await.cache.invalidate_all();
        // Run a refresh_delta in the background to pick up sibling files
        // a user might have changed outside the editor (git pull, etc.).
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let st = state.blocking_read();
            let store_guard = st.store.lock().unwrap();
            if let Some(store) = store_guard.as_ref() {
                let _ = index::refresh(&st.repo_root, store);
                // Graph is now potentially stale.
                drop(store_guard);
                *st.graph.lock().unwrap() = None;
            }
        });
        let _ = p; // saved file was already re-indexed by did_change/did_open.
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
        let mut st = self.state.write().await;
        st.open_docs.remove(&p.text_document.uri);
    }

    async fn document_symbol(
        &self,
        p: DocumentSymbolParams,
    ) -> RpcResult<Option<DocumentSymbolResponse>> {
        let st = self.state.read().await;
        let rel = match handlers::rel_from_url(&st.repo_root, &p.text_document.uri) {
            Some(r) => r,
            None => return Ok(None),
        };

        let key = CacheKey::DocumentSymbols(rel.clone());
        if let Some(v) = st.cache.get(&key) {
            if let Ok(parsed) = serde_json::from_value::<Vec<DocumentSymbol>>((*v).clone()) {
                return Ok(Some(DocumentSymbolResponse::Nested(parsed)));
            }
        }

        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let syms = store.symbols_in_file(&rel).unwrap_or_default();
        let dsyms = handlers::document_symbols(syms);
        if let Ok(v) = serde_json::to_value(&dsyms) {
            st.cache.put(key, StdArc::new(v));
        }
        Ok(Some(DocumentSymbolResponse::Nested(dsyms)))
    }

    async fn goto_definition(
        &self,
        p: GotoDefinitionParams,
    ) -> RpcResult<Option<GotoDefinitionResponse>> {
        let st = self.state.read().await;
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };

        let key = CacheKey::Definition(word.clone());
        if let Some(v) = st.cache.get(&key) {
            if let Ok(parsed) = serde_json::from_value::<Vec<Location>>((*v).clone()) {
                return Ok(if parsed.is_empty() {
                    None
                } else {
                    Some(GotoDefinitionResponse::Array(parsed))
                });
            }
        }

        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let hits = find_symbol(store, &word).unwrap_or_default();
        let locs = handlers::definition_locations(&st.repo_root, hits);
        if let Ok(v) = serde_json::to_value(&locs) {
            st.cache.put(key, StdArc::new(v));
        }
        if locs.is_empty() {
            return Ok(None);
        }
        Ok(Some(GotoDefinitionResponse::Array(locs)))
    }

    async fn references(&self, p: ReferenceParams) -> RpcResult<Option<Vec<Location>>> {
        let st = self.state.read().await;
        let uri = &p.text_document_position.text_document.uri;
        let pos = p.text_document_position.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };
        let root = st.repo_root.clone();
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };

        // crabcc-core's `find_refs` only covers JS/TS/Ruby. For everything
        // else (Rust, Python, Swift) the call-edge index from
        // `find_callers` is the authoritative source. We union both so
        // language coverage is the same set as our indexer.
        let mut hits = query::find_refs(store, &root, &word).unwrap_or_default();
        let mut callers = query::find_callers(store, &root, &word).unwrap_or_default();
        hits.append(&mut callers);
        // Dedup by (file, line, col). The two sources can overlap on JS/TS.
        hits.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line.cmp(&b.line))
                .then(a.col.cmp(&b.col))
        });
        hits.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.col == b.col);
        Ok(Some(handlers::reference_locations(&root, hits)))
    }

    async fn hover(&self, p: HoverParams) -> RpcResult<Option<Hover>> {
        let st = self.state.read().await;
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };

        let key = CacheKey::Hover(word.clone());
        if let Some(v) = st.cache.get(&key) {
            if let Ok(parsed) = serde_json::from_value::<Option<Hover>>((*v).clone()) {
                return Ok(parsed);
            }
        }

        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let hits = find_symbol(store, &word).unwrap_or_default();
        let h = handlers::hover_for(&hits);
        if let Ok(v) = serde_json::to_value(&h) {
            st.cache.put(key, StdArc::new(v));
        }
        Ok(h)
    }

    async fn symbol(
        &self,
        p: WorkspaceSymbolParams,
    ) -> RpcResult<Option<Vec<SymbolInformation>>> {
        let q = p.query;
        if q.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let st = self.state.read().await;
        let key = CacheKey::WorkspaceSymbol {
            query: q.clone(),
            limit: 200,
        };
        if let Some(v) = st.cache.get(&key) {
            if let Ok(parsed) = serde_json::from_value::<Vec<SymbolInformation>>((*v).clone()) {
                return Ok(Some(parsed));
            }
        }
        let root = st.repo_root.clone();
        let fts_guard = st.fts.lock().unwrap();
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let mut syms = Vec::new();
        if let Some(fts) = fts_guard.as_ref() {
            for hit in fts.prefix(&q, 50).unwrap_or_default() {
                // The Fts hit has name + file; re-hydrate via find_symbol
                // to keep one wire shape across the surface.
                if let Ok(mut found) = find_symbol(store, &hit.name) {
                    syms.append(&mut found);
                }
            }
        } else {
            syms = find_symbol(store, &q).unwrap_or_default();
        }
        // De-dup by (name, file, line) to avoid n-of-the-same when fts and store overlap.
        syms.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.file.cmp(&b.file))
                .then(a.line_start.cmp(&b.line_start))
        });
        syms.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.line_start == b.line_start);
        syms.truncate(200);
        let out = handlers::workspace_symbol_legacy(&root, syms);
        if let Ok(v) = serde_json::to_value(&out) {
            st.cache.put(key, StdArc::new(v));
        }
        Ok(Some(out))
    }

    async fn prepare_call_hierarchy(
        &self,
        p: CallHierarchyPrepareParams,
    ) -> RpcResult<Option<Vec<CallHierarchyItem>>> {
        let st = self.state.read().await;
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let syms = find_symbol(store, &word).unwrap_or_default();
        let items: Vec<_> = syms
            .iter()
            .filter_map(|s| handlers::call_hierarchy_item(&st.repo_root, s))
            .collect();
        Ok(Some(items))
    }

    async fn incoming_calls(
        &self,
        p: CallHierarchyIncomingCallsParams,
    ) -> RpcResult<Option<Vec<CallHierarchyIncomingCall>>> {
        let st = self.state.read().await;
        let name = p.item.name.clone();
        let root = st.repo_root.clone();
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let hits = find_callers(store, &root, &name).unwrap_or_default();
        Ok(Some(handlers::incoming_calls(&root, &name, hits)))
    }

    async fn outgoing_calls(
        &self,
        p: CallHierarchyOutgoingCallsParams,
    ) -> RpcResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        let st = self.state.read().await;
        let name = p.item.name.clone();
        let root = st.repo_root.clone();

        // Ensure the call graph is built.
        let need_build = st.graph.lock().unwrap().is_none();
        if need_build {
            let store_guard = st.store.lock().unwrap();
            if let Some(store) = store_guard.as_ref() {
                if let Ok(g) = CallGraph::build_from_edges(store) {
                    *st.graph.lock().unwrap() = Some(g);
                }
            }
        }
        let graph_guard = st.graph.lock().unwrap();
        let graph = match graph_guard.as_ref() {
            Some(g) => g,
            None => return Ok(Some(Vec::new())),
        };
        let callees: Vec<String> = graph
            .callees
            .get(&name)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        drop(graph_guard);

        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(Some(Vec::new())),
        };
        let mut targets = Vec::new();
        for callee in callees.iter().take(200) {
            if let Ok(syms) = find_symbol(store, callee) {
                targets.extend(syms);
            }
        }
        Ok(Some(handlers::outgoing_calls(&root, targets)))
    }

    async fn execute_command(
        &self,
        p: ExecuteCommandParams,
    ) -> RpcResult<Option<serde_json::Value>> {
        let st = self.state.read().await;
        let root = st.repo_root.clone();
        drop(st);
        let out: anyhow::Result<serde_json::Value> = match p.command.as_str() {
            commands::MEMORY_SEARCH => commands::memory_search(&root, &p.arguments),
            commands::WEBFETCH => commands::webfetch(&p.arguments),
            commands::RERANK => commands::rerank(&p.arguments),
            other => Err(anyhow::anyhow!("unknown command: {other}")),
        };
        match out {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                let _ = self
                    .client
                    .log_message(MessageType::ERROR, format!("executeCommand: {e:#}"))
                    .await;
                Ok(Some(serde_json::json!({"error": e.to_string()})))
            }
        }
    }
}

impl Backend {
    /// Index a single open document into the SQLite store. Crabcc-core
    /// languages go through `extract::extract_file_with_edges`; Swift goes
    /// through our local extractor.
    async fn index_uri(&self, uri: &Url, src: &str, root: &std::path::Path) {
        let rel = match handlers::rel_from_url(root, uri) {
            Some(r) => r,
            None => return,
        };

        let result: AResult<()> = tokio::task::spawn_blocking({
            let rel = rel.clone();
            let src = src.to_string();
            let state = self.state.clone();
            move || -> AResult<()> {
                let st = state.blocking_read();
                let store_guard = st.store.lock().unwrap();
                let store = match store_guard.as_ref() {
                    Some(s) => s,
                    None => return Ok(()),
                };
                let sha = hash::sha256_hex(src.as_bytes());
                let mtime = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                #[cfg(feature = "swift")]
                if matches!(Lang::from_path(std::path::Path::new(&rel)), Some(Lang::Swift)) {
                    let (syms, edges) = crate::swift::extract(&rel, &src)?;
                    let fid = store.upsert_file(&rel, &sha, mtime, "swift")?;
                    store.replace_symbols(fid, &syms)?;
                    store.replace_edges(fid, &edges)?;
                    return Ok(());
                }

                // crabcc-core languages: delegate to its extractor.
                if let Some(detected) = crabcc_core::extract::detect_lang(std::path::Path::new(&rel)) {
                    let (syms, edges) = crabcc_core::extract::extract_file_with_edges(
                        &rel, &src, detected,
                    )?;
                    let fid = store.upsert_file(&rel, &sha, mtime, detected)?;
                    store.replace_symbols(fid, &syms)?;
                    store.replace_edges(fid, &edges)?;
                }
                Ok(())
            }
        })
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("join: {e}")));

        if let Err(e) = result {
            warn!(target: "ucracc_lsp", ?uri, error = %e, "index_uri failed");
        }
    }
}

fn word_at(state: &State, uri: &Url, pos: Position) -> Option<String> {
    let text = state.open_docs.get(uri)?;
    let line = text.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    let bytes = line.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if col > bytes.len() {
        return None;
    }
    let mut start = col;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(line[start..end].to_string())
}
