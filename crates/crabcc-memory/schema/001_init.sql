PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS wings (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    kind        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS rooms (
    id          INTEGER PRIMARY KEY,
    wing_id     INTEGER NOT NULL REFERENCES wings(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    UNIQUE(wing_id, name)
);

-- Sessions group drawers by the invocation that created them. `id` is
-- typically `$TERM_SESSION_ID` (macOS) or a generated UUID; `cwd` /
-- `git_branch` / `git_sha` are recorded at session start so later queries
-- can filter by "what did I capture from terminal X" or "from branch Y".
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    cwd         TEXT,
    git_branch  TEXT,
    git_sha     TEXT
);

CREATE TABLE IF NOT EXISTS drawers (
    id          INTEGER PRIMARY KEY,
    wing_id     INTEGER NOT NULL REFERENCES wings(id) ON DELETE CASCADE,
    room_id     INTEGER          REFERENCES rooms(id) ON DELETE SET NULL,
    session_id  TEXT             REFERENCES sessions(id) ON DELETE SET NULL,
    source_id   TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    sha256      TEXT NOT NULL,
    -- 0 = plain UTF-8, 1 = FSST-encoded via crabcc_core::compress::Codec.
    -- sha256 above is ALWAYS computed on the plaintext body — encoding does
    -- not change the dedup identity of a drawer.
    body_enc    INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_id, sha256)
);

-- Embeddings stored as raw f32 blobs (4 bytes × dim). M0.5 will add a
-- sqlite-vec virtual table alongside this for ANN search; M0 does brute-
-- force cosine over this table directly. Same row layout, additive change.
CREATE TABLE IF NOT EXISTS drawer_embeddings (
    drawer_id INTEGER PRIMARY KEY REFERENCES drawers(id) ON DELETE CASCADE,
    dim       INTEGER NOT NULL,
    bytes     BLOB    NOT NULL
);

-- Knowledge graph schema is included now so the file is forward-compatible.
-- KG operations are M4 work — these tables stay empty until then.
CREATE TABLE IF NOT EXISTS kg_triples (
    id               INTEGER PRIMARY KEY,
    subject          TEXT NOT NULL,
    predicate        TEXT NOT NULL,
    object           TEXT NOT NULL,
    valid_from       INTEGER NOT NULL,
    valid_to         INTEGER,
    source_drawer_id INTEGER REFERENCES drawers(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_drawers_wing      ON drawers(wing_id);
CREATE INDEX IF NOT EXISTS idx_drawers_room      ON drawers(room_id);
CREATE INDEX IF NOT EXISTS idx_drawers_sha       ON drawers(sha256);
CREATE INDEX IF NOT EXISTS idx_drawers_session   ON drawers(session_id);
CREATE INDEX IF NOT EXISTS idx_kg_subject        ON kg_triples(subject);
CREATE INDEX IF NOT EXISTS idx_kg_predicate      ON kg_triples(predicate);
CREATE INDEX IF NOT EXISTS idx_kg_valid_from     ON kg_triples(valid_from);

-- Default wing — created lazily on first drawer insert if absent. Schema
-- doesn't seed it because INSERT OR IGNORE there would race with concurrent
-- callers; the Backend handles the get-or-create dance under a transaction.
