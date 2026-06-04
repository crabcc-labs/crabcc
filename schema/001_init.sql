-- crabcc index schema (v4) — symbol-ID-keyed edges, unresolved sentinel.
-- Mirrors codeindex.cc shape: files keyed by sha256; symbols + edges normalized.

PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS files (
    id         INTEGER PRIMARY KEY,
    path       TEXT    NOT NULL UNIQUE,
    sha256     TEXT    NOT NULL,
    mtime      INTEGER NOT NULL,
    lang       TEXT    NOT NULL,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id            INTEGER PRIMARY KEY,
    file_id       INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    qualified     TEXT,                          -- "crate::module::Foo" when extractable
    kind          TEXT    NOT NULL,              -- function|method|class|struct|enum|trait|const|var|type|sentinel
    parent_id     INTEGER REFERENCES symbols(id),
    line_start    INTEGER NOT NULL,
    line_end      INTEGER NOT NULL,
    signature     TEXT,
    -- 0 = plain UTF-8, 1 = FSST-encoded
    signature_enc INTEGER NOT NULL DEFAULT 0,
    visibility    TEXT
);

CREATE INDEX IF NOT EXISTS idx_symbols_name      ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file      ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_kind      ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_qual      ON symbols(qualified);
CREATE INDEX IF NOT EXISTS idx_symbols_file_line ON symbols(file_id, line_start);
CREATE INDEX IF NOT EXISTS idx_symbols_name_kind ON symbols(name, kind);
CREATE INDEX IF NOT EXISTS idx_files_lang        ON files(lang);

-- v4: edges are symbol-ID FKs. dst_symbol_id may point at a sentinel row in
-- symbols where kind='sentinel' (for unresolved names, see unresolved_names).
--
-- WITHOUT ROWID: the composite PK (src, dst, kind, line) is the clustered
-- B-tree key — no hidden rowid column. Lookups by src_symbol_id use the PK
-- prefix for free; dst and (dst, kind) still need explicit secondary indices.
-- Eliminates ~30 % of edge-table storage vs the rowid-table shape.
CREATE TABLE IF NOT EXISTS edges (
    src_symbol_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    dst_symbol_id   INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL CHECK (kind IN ('call','ref','import','inherit','impl')),
    line            INTEGER NOT NULL,
    PRIMARY KEY (src_symbol_id, dst_symbol_id, kind, line)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_edges_dst       ON edges(dst_symbol_id);
CREATE INDEX IF NOT EXISTS idx_edges_dst_kind  ON edges(dst_symbol_id, kind);

-- Sentinel pattern for unresolved edges (languages without a resolver yet,
-- or genuinely ambiguous calls). One sentinel symbol row per unique name;
-- queries that want recall can union edges through these.
CREATE TABLE IF NOT EXISTS unresolved_names (
    symbol_id INTEGER PRIMARY KEY REFERENCES symbols(id) ON DELETE CASCADE,
    name      TEXT    NOT NULL UNIQUE
);
CREATE INDEX IF NOT EXISTS idx_unresolved_name ON unresolved_names(name);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '4');
