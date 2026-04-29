# Indexing & refresh

## First-time index

```bash
crabcc index
```

```json
{"files_indexed":13057,"symbols":38214,"skipped_unsupported":2188,
 "skipped_too_large":4,"skipped_unreadable":0,"skipped_parse_error":12}
```

- **`files_indexed`** — the indexed count (your TS/JS/Ruby files).
- **`symbols`** — total symbols extracted across all files.
- **`skipped_unsupported`** — files in unsupported languages (markdown, yaml, …).
- **`skipped_too_large`** — files over 2 MB (skipped for sanity).
- **`skipped_parse_error`** — files where tree-sitter couldn't parse.

`crabcc index` also rebuilds the Tantivy fuzzy/prefix sidecar at `.crabcc/tantivy/`.

## Incremental refresh

```bash
crabcc refresh
```

```json
{"new":3,"reindexed":12,"touched":1,"unchanged":13280,"deleted":0,
 "skipped_unsupported":0,"skipped_too_large":0,"skipped_unreadable":0,
 "skipped_parse_error":0}
```

- mtime-unchanged files are skipped without reading the file (cheapest path).
- mtime-changed files are hashed; if the hash matches stored, only mtime is updated
  (`touched`).
- Hash mismatch → reparse + replace symbols (`reindexed`).
- New files on disk → indexed (`new`).
- Files gone from disk → row deleted (`deleted`).

Wall-time on mc-mothership (~13k files): ~250 ms no-op, ~700 ms when one file changed.

## Tantivy sidecar

`crabcc refresh` does **not** rebuild Tantivy — only the SQLite index. If your
fuzzy/prefix results lag the SQLite store, run:

```bash
crabcc fts-rebuild
```

`crabcc index` rebuilds Tantivy automatically — use that when in doubt.

## Where things live

```
.crabcc/
├── index.db          # SQLite (files, symbols, edges)
└── tantivy/          # Tantivy index (fuzzy + prefix on symbol names)
```

Add `.crabcc/` to your repo's `.gitignore`.

## Performance notes

- Indexing parallelism: currently single-threaded. Roughly 13k files → 5–30 s
  depending on disk and CPU.
- Skipped languages: anything not TS/TSX/JS/Ruby. Adding a language is a tree-sitter
  grammar + an extractor in `crates/crabcc-core/src/extract.rs`.
- Max file size: 2 MB hard cap. Larger files are skipped (sanity guard against
  generated minified bundles).
