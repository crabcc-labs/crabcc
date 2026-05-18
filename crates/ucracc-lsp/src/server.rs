use crate::cache::{Key as CacheKey, LruCache, Value as CacheValue};
use crate::commands;
use crate::handlers;
use crate::lang::{Lang, SUPPORTED_LANGUAGE_IDS};
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
use std::sync::Arc as StdArc;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{Mutex, RwLock};
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};
use tracing::{info, warn};

pub struct RootConfig {
    pub repo_root: PathBuf,
    pub db_path: PathBuf,
    pub fts_dir: PathBuf,
}

pub struct Backend {
    pub client: Client,
    pub root_config: Mutex<Arc<RootConfig>>,
    /// Read-through LRU for repeated identical queries. Lock-free
    /// (moka::sync) — hoisted out of `State` so cache hits don't pay
    /// the `state.read().await` cost. Flushed on every write event.
    pub cache: Arc<LruCache>,
    pub state: Arc<RwLock<State>>,
}

pub struct State {
    /// SQLite store. Behind a sync Mutex because rusqlite::Connection is
    /// !Sync; we never hold it across `.await`. `None` until the first
    /// handler that needs it triggers `ensure_store`.
    pub store: std::sync::Mutex<Option<Store>>,
    pub fts: std::sync::Mutex<Option<Fts>>,
    pub graph: std::sync::Mutex<Option<CallGraph>>,
    /// In-memory mirror of open documents; LSP gives us deltas but the
    /// indexer needs the full source.
    pub open_docs: HashMap<Url, String>,
    /// Per-document parsed `Tree`, kept alongside the source so we can
    /// hand the old tree to `parse(src, Some(&old))` on `didChange` and
    /// let tree-sitter reuse unchanged subtrees.
    pub trees: std::sync::Mutex<HashMap<Url, tree_sitter::Tree>>,
}

impl State {
    /// Open the SQLite store on first call; cheap no-op on subsequent
    /// calls. Returns `true` if the store is now available.
    pub fn ensure_store(&self, db_path: &std::path::Path) -> bool {
        let mut g = self.store.lock().unwrap();
        if g.is_some() {
            return true;
        }
        if !db_path.exists() {
            return false;
        }
        match Store::open(db_path) {
            Ok(s) => {
                *g = Some(s);
                true
            }
            Err(e) => {
                tracing::warn!(target: "ucracc_lsp", error = %e, "ensure_store failed");
                false
            }
        }
    }

    /// Same idempotent lazy-open contract for the tantivy sidecar.
    pub fn ensure_fts(&self, fts_dir: &std::path::Path) -> bool {
        let mut g = self.fts.lock().unwrap();
        if g.is_some() {
            return true;
        }
        if !fts_dir.exists() {
            return false;
        }
        match Fts::open(fts_dir) {
            Ok(f) => {
                *g = Some(f);
                true
            }
            Err(e) => {
                tracing::warn!(target: "ucracc_lsp", error = %e, "ensure_fts failed");
                false
            }
        }
    }
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            root_config: Mutex::new(Arc::new(RootConfig {
                repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                db_path: PathBuf::new(),
                fts_dir: PathBuf::new(),
            })),
            cache: Arc::new(LruCache::new()),
            state: Arc::new(RwLock::new(State {
                store: std::sync::Mutex::new(None),
                fts: std::sync::Mutex::new(None),
                graph: std::sync::Mutex::new(None),
                open_docs: HashMap::new(),
                trees: std::sync::Mutex::new(HashMap::new()),
            })),
        }
    }

    fn server_capabilities() -> ServerCapabilities {
        ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::INCREMENTAL,
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
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|wf| wf.first().and_then(|f| f.uri.to_file_path().ok()))
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        // Record paths; do NOT open Store/Fts yet. They're lazy-opened
        // on first use (or prefetched from the `initialized` notification
        // below). This keeps `initialize` in the tens-of-microseconds
        // range instead of paying the ~1 ms SQLite/tantivy open cost
        // before the editor has even sent a request.
        let mut cfg = self.root_config.lock().await;
        *cfg = Arc::new(RootConfig {
            repo_root: root.clone(),
            db_path: root.join(".crabcc/index.db"),
            fts_dir: root.join(".crabcc/tantivy"),
        });
        drop(cfg);

        info!(
            target: "ucracc_lsp",
            root = %root.display(),
            langs = SUPPORTED_LANGUAGE_IDS.len(),
            "initialized (lazy)"
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
        // Prefetch the store + fts in the background so the first hover
        // / definition request doesn't pay the cold-open cost. If the
        // user never sends one, no I/O ever happens.
        let state = self.state.clone();
        let cfg = self.root_config.lock().await.clone();
        let prefetch = tokio::task::spawn_blocking(move || {
            let st = state.blocking_read();
            let store_ok = st.ensure_store(&cfg.db_path);
            let _ = st.ensure_fts(&cfg.fts_dir);
            (store_ok, cfg.repo_root.clone())
        })
        .await;

        let (store_ok, repo_root) = match prefetch {
            Ok(t) => t,
            Err(_) => return,
        };
        if !store_ok {
            warn!(target: "ucracc_lsp", "no .crabcc/index.db at {} — run `crabcc index` first", repo_root.display());
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
        let cfg = self.root_config.lock().await.clone();
        let mut st = self.state.write().await;
        st.open_docs
            .insert(p.text_document.uri.clone(), p.text_document.text.clone());
        self.cache.invalidate_all();
        drop(st);
        self.index_uri(&p.text_document.uri, &p.text_document.text, &cfg.repo_root)
            .await;
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        // INCREMENTAL sync — each event has a `range`. We apply changes
        // to the in-memory mirror AND to the cached parse tree so the
        // next parse can reuse subtrees outside the touched region.
        let cfg = self.root_config.lock().await.clone();
        let uri = p.text_document.uri.clone();
        let mut text = {
            let st = self.state.read().await;
            st.open_docs.get(&uri).cloned().unwrap_or_default()
        };

        // Apply each change to the in-memory text and accumulate edits
        // to apply to the cached tree.
        let mut tree_edits = Vec::with_capacity(p.content_changes.len());
        let mut had_full_replace = false;
        for change in p.content_changes {
            if change.range.is_none() {
                // Full replace event — drop the tree, replace the text.
                text = change.text;
                had_full_replace = true;
                tree_edits.clear();
            } else if let Some(edit) = crate::incremental::apply_change(&mut text, &change) {
                tree_edits.push(edit);
            }
        }

        let final_text = text.clone();
        let mut st = self.state.write().await;
        st.open_docs.insert(uri.clone(), final_text.clone());
        self.cache.invalidate_all();

        if had_full_replace {
            st.trees.lock().unwrap().remove(&uri);
        } else if !tree_edits.is_empty() {
            // Apply the edits to the cached tree so the next reparse
            // can pick up subtrees outside the changed range.
            let mut trees = st.trees.lock().unwrap();
            if let Some(t) = trees.get_mut(&uri) {
                for edit in &tree_edits {
                    t.edit(edit);
                }
            }
        }
        drop(st);
        self.index_uri(&uri, &final_text, &cfg.repo_root).await;
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        let cfg = self.root_config.lock().await.clone();
        self.cache.invalidate_all();
        // Run a refresh_delta in the background to pick up sibling files
        // a user might have changed outside the editor (git pull, etc.).
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let st = state.blocking_read();
            st.ensure_store(&cfg.db_path);
            let store_guard = st.store.lock().unwrap();
            if let Some(store) = store_guard.as_ref() {
                let _ = index::refresh(&cfg.repo_root, store);
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
        st.trees.lock().unwrap().remove(&p.text_document.uri);
    }

    async fn document_symbol(
        &self,
        p: DocumentSymbolParams,
    ) -> RpcResult<Option<DocumentSymbolResponse>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let rel = match handlers::rel_from_url(&cfg.repo_root, &p.text_document.uri) {
            Some(r) => r,
            None => return Ok(None),
        };

        let key = CacheKey::DocumentSymbols(rel.clone());
        if let Some(CacheValue::DocumentSymbols(dsyms)) = self.cache.get(&key) {
            return Ok(Some(DocumentSymbolResponse::Nested((*dsyms).clone())));
        }

        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let syms = store.symbols_in_file(&rel).unwrap_or_default();
        let dsyms = handlers::document_symbols(syms);
        self.cache.put(
            key,
            CacheValue::DocumentSymbols(StdArc::new(dsyms.clone())),
        );
        Ok(Some(DocumentSymbolResponse::Nested(dsyms)))
    }

    async fn goto_definition(
        &self,
        p: GotoDefinitionParams,
    ) -> RpcResult<Option<GotoDefinitionResponse>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };

        let key = CacheKey::Definition(word.clone());
        if let Some(CacheValue::Definition(locs)) = self.cache.get(&key) {
            return Ok(if locs.is_empty() {
                None
            } else {
                Some(GotoDefinitionResponse::Array((*locs).clone()))
            });
        }

        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let hits = find_symbol(store, &word).unwrap_or_default();
        let locs = handlers::definition_locations(&cfg.repo_root, hits);
        self.cache
            .put(key, CacheValue::Definition(StdArc::new(locs.clone())));
        if locs.is_empty() {
            return Ok(None);
        }
        Ok(Some(GotoDefinitionResponse::Array(locs)))
    }

    async fn references(&self, p: ReferenceParams) -> RpcResult<Option<Vec<Location>>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let uri = &p.text_document_position.text_document.uri;
        let pos = p.text_document_position.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Check cache first
        let cache_key = CacheKey::References(word.clone());
        if let Some(CacheValue::References(locs)) = self.cache.get(&cache_key) {
            return Ok(Some((*locs).clone()));
        }

        let root = cfg.repo_root.clone();
        // Get relative path to detect language
        let rel = match handlers::rel_from_url(&root, uri) {
            Some(r) => r,
            None => return Ok(None),
        };
        // Gate find_refs to languages that support edge-based refs
        let lang = crabcc_core::extract::detect_lang(std::path::Path::new(&rel));
        let do_find_refs = lang.is_some_and(|l| {
            matches!(l, "typescript" | "tsx" | "javascript" | "ruby")
        });

        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };

        // Only call find_refs for supported languages
        let mut hits = if do_find_refs {
            query::find_refs(store, &root, &word).unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut callers = query::find_callers(store, &root, &word).unwrap_or_default();
        hits.append(&mut callers);
        // Dedup by (file, line, col)
        hits.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line.cmp(&b.line))
                .then(a.col.cmp(&b.col))
        });
        hits.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.col == b.col);
        let locs = handlers::reference_locations(&root, hits);
        self.cache
            .put(cache_key, CacheValue::References(StdArc::new(locs.clone())));
        Ok(Some(locs))
    }

    async fn hover(&self, p: HoverParams) -> RpcResult<Option<Hover>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };

        let key = CacheKey::Hover(word.clone());
        if let Some(CacheValue::Hover(h)) = self.cache.get(&key) {
            return Ok((*h).clone());
        }

        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let hits = find_symbol(store, &word).unwrap_or_default();
        let h = handlers::hover_for(&hits);
        self.cache
            .put(key, CacheValue::Hover(StdArc::new(h.clone())));
        Ok(h)
    }

    async fn symbol(&self, p: WorkspaceSymbolParams) -> RpcResult<Option<Vec<SymbolInformation>>> {
        let q = p.query;
        if q.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let key = CacheKey::WorkspaceSymbol {
            query: q.clone(),
            limit: 200,
        };
        if let Some(CacheValue::WorkspaceSymbol(out)) = self.cache.get(&key) {
            return Ok(Some((*out).clone()));
        }
        st.ensure_fts(&cfg.fts_dir);
        st.ensure_store(&cfg.db_path);
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
        let out = handlers::workspace_symbol_legacy(&cfg.repo_root, syms);
        self.cache.put(
            key,
            CacheValue::WorkspaceSymbol(StdArc::new(out.clone())),
        );
        Ok(Some(out))
    }

    async fn prepare_call_hierarchy(
        &self,
        p: CallHierarchyPrepareParams,
    ) -> RpcResult<Option<Vec<CallHierarchyItem>>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;
        let word = match word_at(&st, uri, pos) {
            Some(w) => w,
            None => return Ok(None),
        };
        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let syms = find_symbol(store, &word).unwrap_or_default();
        let items: Vec<_> = syms
            .iter()
            .filter_map(|s| handlers::call_hierarchy_item(&cfg.repo_root, s))
            .collect();
        Ok(Some(items))
    }

    async fn incoming_calls(
        &self,
        p: CallHierarchyIncomingCallsParams,
    ) -> RpcResult<Option<Vec<CallHierarchyIncomingCall>>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let name = p.item.name.clone();
        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let hits = find_callers(store, &cfg.repo_root, &name).unwrap_or_default();
        Ok(Some(handlers::incoming_calls(&cfg.repo_root, &name, hits)))
    }

    async fn outgoing_calls(
        &self,
        p: CallHierarchyOutgoingCallsParams,
    ) -> RpcResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        let cfg = self.root_config.lock().await.clone();
        let st = self.state.read().await;
        let name = p.item.name.clone();

        // Ensure the call graph is built.
        let need_build = st.graph.lock().unwrap().is_none();
        if need_build {
            st.ensure_store(&cfg.db_path);
            let store_guard = st.store.lock().unwrap();
            if let Some(store) = store_guard.as_ref() {
                if let Ok(g) = CallGraph::build(store, &cfg.repo_root) {
                    *st.graph.lock().unwrap() = Some(g);
                }
            }
        }

        // v4: graph keys are symbol_ids (i64), not names. Resolve the
        // requested name to a SymbolId, walk callees as ids, then resolve
        // each callee id back to a name string for the existing
        // find_symbol-by-name path below.
        st.ensure_store(&cfg.db_path);
        let store_guard = st.store.lock().unwrap();
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return Ok(Some(Vec::new())),
        };

        let name_id = match store.symbol_id_by_name(&name).ok().flatten() {
            Some(id) => id,
            None => return Ok(Some(Vec::new())),
        };

        let graph_guard = st.graph.lock().unwrap();
        let graph = match graph_guard.as_ref() {
            Some(g) => g,
            None => return Ok(Some(Vec::new())),
        };
        let callee_ids: Vec<i64> = graph
            .callees
            .get(&name_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        drop(graph_guard);

        let callees: Vec<String> = callee_ids
            .iter()
            .filter_map(|id| store.symbol_name_by_id(*id).ok().flatten())
            .collect();
        let mut targets = Vec::new();
        for callee in callees.iter().take(200) {
            if let Ok(syms) = find_symbol(store, callee) {
                targets.extend(syms);
            }
        }
        Ok(Some(handlers::outgoing_calls(&cfg.repo_root, targets)))
    }

    async fn execute_command(
        &self,
        p: ExecuteCommandParams,
    ) -> RpcResult<Option<serde_json::Value>> {
        let cfg = self.root_config.lock().await.clone();
        let out: anyhow::Result<serde_json::Value> = match p.command.as_str() {
            commands::MEMORY_SEARCH => commands::memory_search(&cfg.repo_root, &p.arguments),
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

        let cfg = self.root_config.lock().await.clone();
        let uri_owned = uri.clone();
        let result: AResult<()> = tokio::task::spawn_blocking({
            let rel = rel.clone();
            let src = src.to_string();
            let state = self.state.clone();
            let uri = uri_owned;
            move || -> AResult<()> {
                let st = state.blocking_read();
                st.ensure_store(&cfg.db_path);
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

                let lang = Lang::from_path(std::path::Path::new(&rel));
                if let Some(l) = lang {
                    if l.handled_internally() {
                        let (syms, edges, lang_tag) = match l {
                            #[cfg(feature = "yaml")]
                            Lang::Yaml => {
                                let (s, e) = crate::yaml::extract(&rel, &src)?;
                                (s, e, "yaml")
                            }
                            #[cfg(feature = "markdown")]
                            Lang::Markdown => {
                                let (s, e) = crate::markdown::extract(&rel, &src)?;
                                (s, e, "markdown")
                            }
                            // If a `handled_internally` variant is reached
                            // with its feature disabled, fall through to the
                            // crabcc-core path which will skip it.
                            _ => return Ok(()),
                        };
                        let fid = store.upsert_file(&rel, &sha, mtime, lang_tag)?;
                        store.replace_symbols(fid, &syms)?;
                        store.replace_edges(fid, &edges)?;
                        return Ok(());
                    }
                }

                // crabcc-core languages (now including Swift and Bash):
                // delegate to its extractor. We drive the parser
                // ourselves so we can reuse the cached `Tree` from the
                // last edit — `parser.parse(src, Some(&old_tree))` lets
                // tree-sitter skip subtrees outside the InputEdit
                // ranges that `did_change` already applied to that tree.
                if let Some(detected) =
                    crabcc_core::extract::detect_lang(std::path::Path::new(&rel))
                {
                    let ts_lang = crabcc_core::extract::language(detected)?;
                    let mut parser = tree_sitter::Parser::new();
                    parser
                        .set_language(&ts_lang)
                        .map_err(|e| anyhow::anyhow!("set_language({detected}): {e}"))?;

                    let old_tree = st.trees.lock().unwrap().remove(&uri);
                    let new_tree = parser
                        .parse(&src, old_tree.as_ref())
                        .ok_or_else(|| anyhow::anyhow!("parse failed for {rel}"))?;

                    let (syms, edges) = crabcc_core::extract::extract_from_root(
                        new_tree.root_node(),
                        src.as_bytes(),
                        &rel,
                        detected,
                    );
                    let fid = store.upsert_file(&rel, &sha, mtime, detected)?;
                    store.replace_symbols(fid, &syms)?;
                    store.replace_edges(fid, &edges)?;
                    // Re-cache the freshly-parsed tree.
                    st.trees.lock().unwrap().insert(uri.clone(), new_tree);
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
