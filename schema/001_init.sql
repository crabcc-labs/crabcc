-- crabcc index schema (v1)
-- Mirrors codeindex.cc shape: files keyed by sha256, symbols + edges normalized.

PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS files (
    id        INTEGER PRIMARY KEY,
    path      TEXT    NOT NULL UNIQUE,
    sha256    TEXT    NOT NULL,
    mtime     INTEGER NOT NULL,
    lang      TEXT    NOT NULL,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id         INTEGER PRIMARY KEY,
    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    kind       TEXT    NOT NULL,           -- function|method|class|struct|enum|trait|const|var|type
    signature  TEXT,                        -- compact one-line signature
    parent     TEXT,                        -- enclosing symbol name (nullable)
    line_start INTEGER NOT NULL,
    line_end   INTEGER NOT NULL,
    visibility TEXT                         -- pub|priv|pkg (lang-dependent)
);

CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);

-- references / call edges. src_symbol may be null for top-level usage.
CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY,
    src_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    src_symbol  INTEGER          REFERENCES symbols(id) ON DELETE SET NULL,
    dst_name    TEXT    NOT NULL,           -- resolved later; name-based for v1
    kind        TEXT    NOT NULL,           -- call|import|inherit|impl|ref
    line        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_name);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_file_id);

-- meta (schema version, last full reindex, etc.)
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1');
