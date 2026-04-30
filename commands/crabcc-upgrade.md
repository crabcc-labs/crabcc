---
description: Check GitHub for a newer crabcc release and walk the user through migration + cleanup.
---

# crabcc upgrade

The crabcc repo is **private**, so the GitHub REST API alone can't see its
releases. The CLI shells out to `gh` (which inherits the user's `gh auth login`
credentials) to query the private repo.

## Steps

1. **Verify `gh` is installed and authenticated.** If `gh auth status` fails,
   stop and tell the user:
   `Install gh from https://cli.github.com and run "gh auth login" — crabcc's
   repo is private and the API call needs your token.`

2. **Run the check** (read-only):
   ```
   crabcc upgrade --check --json
   ```
   If the user has the Ollama auth stack from issue #105 installed, also
   pass `--with-stack` to surface upstream image diffs without mutating
   anything:
   ```
   crabcc upgrade --check --with-stack --json
   ```
   The JSON shape is:
   ```json
   {
     "installed": "1.2.0",
     "latest":    { "tag": "v1.3.0", "published_at": "...", "url": "..." },
     "delta":     { "status": "newer", "current": "1.2.0", "latest": "v1.3.0", "kind": "minor" },
     "recommendations": ["..."]
   }
   ```
   Inspect `delta.status`:
   - `"up_to_date"` — report it, stop.
   - `"newer"` — proceed to step 3.
   - `"ahead"` — local build > latest tag (dev build); usually safe to ignore.
   - `"unknown"` — surface `delta.reason` to the user (likely `gh` not authed).

3. **Brief the user** on the upgrade:
   - Read the latest release notes at `latest.url`.
   - For a `kind: "major"` bump, point at `CHANGELOG.md` and call out any
     breaking changes before proceeding.
   - For `kind: "minor"` or `"patch"`, summarize the recommendations field.

4. **Apply** (only after user consent):
   - Update the binary. Pick the path the user actually used:
     - **Cargo / source build**: `cargo install --git https://github.com/peterlodri-sec/crabcc.git --tag <tag> --bin crabcc`
     - **Released binary**: `gh release download <tag> --repo peterlodri-sec/crabcc --pattern "*$(uname -m)*$(uname -s | tr A-Z a-z)*" -O - | tar xz -C ~/.local/bin/`
   - Optional cleanup of local sidecars (lossless — just clears the index/FTS/graph caches):
     ```
     crabcc upgrade --apply --json
     ```
     This runs the same check as `--check`, then `rm`s `.crabcc/{index.db,tantivy/,graph.json}`.
   - **If the user has the Ollama auth stack installed** (issue #105):
     ```
     crabcc upgrade --apply --with-stack --json
     ```
     This adds `docker compose pull && up -d --wait` against
     `~/.crabcc/ollama-stack/` so any updated upstream images
     (Caddy, LiteLLM, Ollama itself) are picked up. Read-only
     variants: `--check --with-stack` shows what *would* change.
   - Reindex:
     ```
     crabcc index
     ```

5. **Confirm**: run `crabcc upgrade --check` again and verify
   `delta.status == "up_to_date"`.

## Notes

- `crabcc upgrade --apply` **only touches `.crabcc/`** — it never replaces the
  binary itself; that's still on the user.
- Repo override: `crabcc upgrade --check --repo <owner>/<repo>` for forks.
- The MCP server exposes the same surface as the `upgrade` tool —
  `{ apply: bool, repo?: string }` — and returns the same JSON shape.
