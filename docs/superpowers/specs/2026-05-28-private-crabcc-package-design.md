# Private crabcc-adjacent package (Nix) — design

## Scope
Design a **fully private** package and repository (no public distribution), enabling:
- One-line installs inside your org via a private Nix flake.
- Versioned, reproducible configuration.
- Optional internal binary distribution (prebuilt bundle) with **cosign** verification.

Assumption (updated): **Private repo + private Nix flake** with an optional cosign-signed bundle for fast installs.

## Goals
- **Reproducible installs** across macOS/Linux.
- **Versioned config** (MCP/Claude hooks/settings) with explicit semver or git pin.
- **Separation**: crabcc can remain open-source, but all adjacent packaging/config lives in a private repository and is never published publicly.
## Non-goals
- Rewriting crabcc build pipeline.
- Bundling secrets in repo or Nix store.
- Enforcing policy beyond install-time configuration.

## Approaches (tradeoffs)
### A) Private Git repo + Nix flake overlay (recommended)
- **What:** A private flake that imports `crabcc` from GitHub (open-source) pinned to a tag/commit and overlays internal config (hooks, MCP config, optional scripts).
- **Pros:** Reproducible, no binary distribution burden, clean separation, easy to update with pin changes.
- **Cons:** Requires Nix; public install path remains separate.

### B) Private binary distribution + Nix derivation
- **What:** Host signed tarballs internally; Nix fetches them. Config lives next to binary in private repo.
- **Pros:** Faster installs, no local Rust toolchain needed.
- **Cons:** Build/signing pipeline required, larger operational burden.

### C) Private config bundle only
- **What:** Keep crabcc public install; private repo only ships MCP/Claude config and wrapper scripts.
- **Pros:** Minimal changes, least ops work.
- **Cons:** Less reproducible; relies on external install procedure and drift.

## Recommended design (A)
### Repository layout
```
crabcc-private/
├── flake.nix
├── flake.lock
├── config/
│   ├── claude/settings.json
│   ├── claude/hooks.json
│   ├── mcp/claude.json
│   └── env/crabcc.env
├── scripts/
│   ├── install.sh
│   └── post-install.sh
└── README.md
```

### Nix flake
- Inputs:
  - `crabcc` pinned to a public tag/commit.
  - `nixpkgs`.
- Outputs:
  - `packages.<system>.crabcc` — the upstream binary (via `crabcc` flake or `cargo` build).
  - `packages.<system>.crabcc-private` — wrapper that installs `crabcc` plus config.
  - `apps.<system>.install` — one-line installer entrypoint.

### Install flow (one-liner)
```
# internal (private)
nix run github:org/crabcc-private#install
```
Behavior:
1. Install `crabcc` (public, pinned).
2. Apply private config to `~/.claude/` and MCP settings.
3. Optionally add hooks and `crabcc` MCP registration.
4. Print next-step `crabcc go` instructions.

### Configuration versioning
- Config is git-versioned in private repo.
- Each release of `crabcc-private` pins crabcc via `flake.lock`.
- No secrets stored; secrets passed via env (e.g., `MCP_AUTH_TOKEN`) or out-of-band.

### Security
- No secrets in repo or Nix store.
- Optional signature verification for upstream tarballs if using B.

### Testing
- Smoke test script in private repo:
  - `crabcc --version`
  - `crabcc info --status-line`
  - `crabcc go` on a fixture repo.

## Open questions
- Confirm assumption A (private flake overlay) vs B/C.
- Which configs are mandatory (Claude hooks, MCP, Cursor/Gemini)?
- Should `crabcc-private` ship an internal MCP server config (HTTP) or only stdio?
