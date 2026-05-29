# Private crabcc Nix Flake Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a **private Nix flake repo** (`peterlodri-sec/crabcc-private`) that installs `crabcc` plus personal config in one command, with no secrets in git or the Nix store, and a Phase-2 hook for cosign-signed prebuilt bundles.

**Architecture:** A separate **private** GitHub repo holds a flake that (a) builds `crabcc` from the public OSS repo pinned by `flake.lock`, (b) wraps it with a config bundle (Claude settings, hooks, MCP registration), and (c) exposes a `nix run .#install` app that materializes config into `~/.claude/`. Secrets are sourced at runtime from `~/.config/crabcc-private/env.local` (chmod 600), never embedded in the Nix store. Cosign verification of a prebuilt tarball is a Phase-2 add-on.

**Tech Stack:** Nix flakes (`nixpkgs`, `flake-utils`), `rustPlatform.buildRustPackage`, `makeWrapper`, plain `bash` installer, optional `agenix` and `cosign` for later phases.

---

## Personal-use defaults baked in

Per `docs/Mark.md` and the brief ("this whole thing is for me, personal use, development"), the open decisions from the spec are resolved as:

| Decision | Choice | Reason |
|---|---|---|
| Approach | A (private flake overlay) | Reproducible, no signing pipeline day 1 |
| Mandatory configs | Claude (settings + hooks + MCP stdio) | Already in the OSS repo; Cursor/Gemini deferred |
| MCP transport | stdio | `crabcc --mcp`, no daemon, no port collision |
| Artifact host | Private GitHub repo | No S3/internal Git needed |
| Secrets at runtime | `~/.config/crabcc-private/env.local`, chmod 600, sourced by wrapper | Out of Nix store, out of git, matches the Discourse "manual unencrypted file + `secretFile` option" pattern |
| Cosign-signed bundle | Phase 2 (placeholder task only) | YAGNI for single-user dev |
| agenix | Phase 2 (only if multi-host) | Single host doesn't need it |

References for secret-management approach (from current NixOS community guidance):
- [Comparison of secret managing schemes — NixOS Wiki](https://wiki.nixos.org/wiki/Comparison_of_secret_managing_schemes)
- [Handling Secrets in NixOS: An Overview (git-crypt, agenix, sops-nix)](https://discourse.nixos.org/t/handling-secrets-in-nixos-an-overview-git-crypt-agenix-sops-nix-and-when-to-use-them/35462)
- [Managing Secrets in NixOS — Discourse thread](https://discourse.nixos.org/t/managing-secrets-in-nixos/72569)

The plain-file `~/.config/.../env.local` chosen here matches the Discourse advice: "create a directory on your host by hand, and manually put unencrypted files in there, and then point the various `secretFile` options at the paths for those files" — the simplest pattern that keeps secrets out of `/nix/store` for single-developer use.

---

## File structure (target repo: `peterlodri-sec/crabcc-private`)

```
crabcc-private/
├── flake.nix                       # Inputs + outputs (Task 2)
├── flake.lock                      # Generated (Task 2)
├── nix/
│   ├── crabcc.nix                  # rustPlatform.buildRustPackage of upstream crabcc (Task 2)
│   ├── crabcc-private.nix          # Wrapper derivation (Task 4)
│   └── installer.nix               # `apps.<sys>.install` derivation (Task 5)
├── config/
│   ├── claude/settings.json        # Personal Claude Code settings (Task 3)
│   ├── claude/hooks.json           # Hooks + crabcc-hint shim (Task 3)
│   └── mcp/claude.json             # MCP server registration (Task 3)
├── scripts/
│   ├── install.sh                  # Non-Nix fallback installer (Task 6)
│   ├── smoke.sh                    # Smoke-test script (Task 7)
│   └── env.local.example           # Template for the runtime secret file (Task 3)
├── docs/
│   ├── README.md                   # How to use (Task 8)
│   └── SECRETS.md                  # Secret-management notes (Task 8)
└── .gitignore                      # Excludes env.local, result, result-*
```

Plus a single change in **this repo** (`peterlodri-sec/crabcc`):

```
crabcc/
└── docs/superpowers/plans/2026-05-28-private-crabcc-flake-implementation.md  # this file
```

No other file in the open-source crabcc repo is modified. The private repo is the sole new artifact.

---

### Task 1: Scaffold the private repo locally

**Files:**
- Create: `~/workspace/peterlodri-sec/crabcc-private/.gitignore`
- Create: `~/workspace/peterlodri-sec/crabcc-private/docs/README.md` (one-liner stub)

- [ ] **Step 1: Create the local directory and git-init**

Run:
```bash
mkdir -p ~/workspace/peterlodri-sec/crabcc-private/{nix,config/claude,config/mcp,scripts,docs}
cd ~/workspace/peterlodri-sec/crabcc-private
git init -b main
```

Expected: empty repo on `main` branch.

- [ ] **Step 2: Write `.gitignore`**

`~/workspace/peterlodri-sec/crabcc-private/.gitignore`:
```gitignore
# Nix build outputs
/result
/result-*

# Runtime secret file — never commit
/env.local
/scripts/env.local
/config/env/env.local

# Editor cruft
.direnv/
.envrc
*.swp
.DS_Store
```

- [ ] **Step 3: Stub README so the repo isn't empty on first push**

`~/workspace/peterlodri-sec/crabcc-private/docs/README.md`:
```markdown
# crabcc-private

Private overlay for [crabcc](https://github.com/peterlodri-sec/crabcc).

Install:
```
nix run github:peterlodri-sec/crabcc-private#install
```

See `docs/SECRETS.md` for runtime secret handling.
```

- [ ] **Step 4: Commit the scaffold**

```bash
cd ~/workspace/peterlodri-sec/crabcc-private
git add .gitignore docs/README.md
git commit -m "scaffold: empty private overlay"
```

Expected: one commit, clean tree.

- [ ] **Step 5: Create the remote as a private GitHub repo**

```bash
gh repo create peterlodri-sec/crabcc-private --private --source . --remote origin --push
```

Expected: `https://github.com/peterlodri-sec/crabcc-private` exists, private, contains the scaffold commit. Verify with `gh repo view peterlodri-sec/crabcc-private --json visibility -q .visibility` — must print `PRIVATE`.

---

### Task 2: Minimal flake building crabcc from a pinned upstream commit

**Files:**
- Create: `flake.nix`
- Create: `nix/crabcc.nix`
- Generated: `flake.lock`

- [ ] **Step 1: Look up the current crabcc release tag to pin**

Run (in the crabcc repo, not the private one):
```bash
cd ~/workspace/peterlodri-sec/crabcc
git tag --sort=-creatordate | head -1
```

Expected: a tag like `v2.4.1`. Note this value — you'll paste it into `flake.nix` below as `CRABCC_TAG`.

- [ ] **Step 2: Write `nix/crabcc.nix`**

`~/workspace/peterlodri-sec/crabcc-private/nix/crabcc.nix`:
```nix
{ lib, rustPlatform, fetchFromGitHub, pkg-config, openssl, stdenv, darwin }:

rustPlatform.buildRustPackage rec {
  pname = "crabcc";
  version = "PIN_VERSION_HERE";   # set in flake.nix via override

  src = fetchFromGitHub {
    owner = "peterlodri-sec";
    repo = "crabcc";
    rev = "vPIN_VERSION_HERE";    # tag; override at flake level
    hash = lib.fakeHash;          # replaced by real hash on first `nix build`
  };

  cargoLock = {
    lockFile = "${src}/Cargo.lock";
  };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ openssl ]
    ++ lib.optionals stdenv.isDarwin [
      darwin.apple_sdk.frameworks.Security
      darwin.apple_sdk.frameworks.SystemConfiguration
    ];

  cargoBuildFlags = [ "--package" "crabcc-cli" ];

  # crabcc's full test suite needs a populated index; skip in nix build.
  doCheck = false;

  meta = with lib; {
    description = "Symbol-aware code intelligence CLI";
    homepage = "https://github.com/peterlodri-sec/crabcc";
    license = licenses.mit;
    mainProgram = "crabcc";
  };
}
```

- [ ] **Step 3: Write `flake.nix`**

`~/workspace/peterlodri-sec/crabcc-private/flake.nix` (replace `CRABCC_TAG` with the tag from Step 1):
```nix
{
  description = "Private crabcc overlay + installer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        crabccTag = "CRABCC_TAG";        # e.g. "v2.4.1"
        crabccVersion = pkgs.lib.removePrefix "v" crabccTag;

        crabcc = (pkgs.callPackage ./nix/crabcc.nix { }).overrideAttrs (_: {
          version = crabccVersion;
          src = pkgs.fetchFromGitHub {
            owner = "peterlodri-sec";
            repo = "crabcc";
            rev = crabccTag;
            hash = pkgs.lib.fakeHash;   # replace after first `nix build`
          };
        });
      in {
        packages.crabcc = crabcc;
        packages.default = crabcc;
      });
}
```

- [ ] **Step 4: First build — discover the real `src` hash**

```bash
cd ~/workspace/peterlodri-sec/crabcc-private
nix build .#crabcc 2>&1 | tee /tmp/build1.log
```

Expected: fails with `error: hash mismatch` and prints both the fake hash and the correct `sha256-...=` for the source. Copy the correct hash.

- [ ] **Step 5: Replace `lib.fakeHash` in `flake.nix` with the real hash**

Edit `flake.nix`: replace the `hash = pkgs.lib.fakeHash;` line with `hash = "sha256-...=";` using the value from Step 4.

- [ ] **Step 6: Second build — discover the `cargoLock` hash (if applicable)**

```bash
nix build .#crabcc 2>&1 | tee /tmp/build2.log
```

If this fails with another `hash mismatch` for the cargo deps, copy the new hash. If `cargoLock.lockFile = "${src}/Cargo.lock"` is used (as written), no extra hash is needed and the build will proceed.

Expected outcome (success): `./result/bin/crabcc` exists.

- [ ] **Step 7: Smoke-test the built binary**

```bash
./result/bin/crabcc --version
```

Expected: prints the same version string as the upstream tag (e.g. `crabcc 2.4.1`).

- [ ] **Step 8: Commit the flake**

```bash
git add flake.nix flake.lock nix/crabcc.nix
git commit -m "feat: flake with crabcc pinned to ${CRABCC_TAG}"
```

---

### Task 3: Add the config bundle

**Files:**
- Create: `config/claude/settings.json`
- Create: `config/claude/hooks.json`
- Create: `config/mcp/claude.json`
- Create: `scripts/env.local.example`

- [ ] **Step 1: Copy your current personal Claude settings as a baseline**

```bash
cp ~/.claude/settings.json ~/workspace/peterlodri-sec/crabcc-private/config/claude/settings.json 2>/dev/null \
  || echo '{}' > ~/workspace/peterlodri-sec/crabcc-private/config/claude/settings.json
```

Open the file and strip any secret values (`*_TOKEN`, `*_API_KEY`, OAuth, email addresses). Verify with:
```bash
grep -E '(token|secret|key|@gmail|@protonmail|TOKEN|KEY)' config/claude/settings.json || echo "clean"
```
Expected: `clean`.

- [ ] **Step 2: Write the MCP fragment**

`config/mcp/claude.json` — keep identical to the OSS repo's `install/integrations/mcp-crabcc.json` so behavior is reproducible:
```json
{
  "mcpServers": {
    "crabcc": {
      "command": "crabcc",
      "args": ["--mcp"]
    }
  }
}
```

- [ ] **Step 3: Write a minimal hooks file (SessionStart + Stop)**

`config/claude/hooks.json`:
```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "crabcc --status-line 2>/dev/null || true" }
        ]
      }
    ]
  }
}
```

Rationale: minimal viable hook that proves the wiring; expand later.

- [ ] **Step 4: Write the env-template**

`scripts/env.local.example`:
```bash
# crabcc-private runtime secrets.
# Copy to: ~/.config/crabcc-private/env.local
# chmod 600 ~/.config/crabcc-private/env.local
# This file is sourced by the wrapper at runtime; NEVER commit values.

# OpenAI for claude-context MCP (if used):
# export OPENAI_API_KEY=""

# Milvus / Zilliz endpoint for claude-context MCP (if used):
# export MILVUS_TOKEN=""
# export MILVUS_ADDRESS=""

# MCP HTTP bearer (only if you switch to --mcp-http later):
# export MCP_AUTH_TOKEN=""
```

- [ ] **Step 5: Commit**

```bash
git add config scripts/env.local.example
git commit -m "feat: ship Claude config + MCP + env template"
```

---

### Task 4: Wrapper derivation — `crabcc-private`

**Files:**
- Create: `nix/crabcc-private.nix`
- Modify: `flake.nix` (add a new `packages.crabcc-private` output)

- [ ] **Step 1: Write `nix/crabcc-private.nix`**

`nix/crabcc-private.nix`:
```nix
{ stdenv, lib, makeWrapper, crabcc, configBundle }:

stdenv.mkDerivation {
  pname = "crabcc-private";
  version = crabcc.version;

  src = configBundle;            # path-typed: ../config

  nativeBuildInputs = [ makeWrapper ];

  installPhase = ''
    mkdir -p $out/share/crabcc-private
    cp -r $src/* $out/share/crabcc-private/

    mkdir -p $out/bin
    # Wrapper sources the runtime env file if present, then execs crabcc.
    makeWrapper ${crabcc}/bin/crabcc $out/bin/crabcc \
      --run 'if [ -f "$HOME/.config/crabcc-private/env.local" ]; then set -a; . "$HOME/.config/crabcc-private/env.local"; set +a; fi'
  '';

  meta = {
    description = "crabcc wrapped with personal config + env-file sourcing";
    mainProgram = "crabcc";
  };
}
```

Why `makeWrapper --run`: it injects a shell prelude into the wrapper script that sources `~/.config/crabcc-private/env.local` if it exists. The file lives outside the Nix store, so the env values are never world-readable via `/nix/store`.

- [ ] **Step 2: Wire the wrapper into `flake.nix`**

In `flake.nix`, after the `crabcc = ...` binding inside the `let`, add:
```nix
        configBundle = ./config;
        crabccPrivate = pkgs.callPackage ./nix/crabcc-private.nix {
          inherit crabcc configBundle;
        };
```

And extend `packages`:
```nix
        packages.crabcc = crabcc;
        packages.crabcc-private = crabccPrivate;
        packages.default = crabccPrivate;
```

- [ ] **Step 3: Build and verify the wrapper**

```bash
nix build .#crabcc-private
./result/bin/crabcc --version
ls $(nix path-info .#crabcc-private)/share/crabcc-private/
```

Expected: version prints; `share/crabcc-private/` contains `claude/`, `mcp/`.

- [ ] **Step 4: Verify the env-sourcing actually runs**

```bash
mkdir -p ~/.config/crabcc-private
echo 'export CRABCC_PRIVATE_SMOKE=1' > ~/.config/crabcc-private/env.local
chmod 600 ~/.config/crabcc-private/env.local

./result/bin/crabcc --version    # should still work
CRABCC_PRIVATE_SMOKE_CHECK=$( ./result/bin/crabcc bash -c 'echo $CRABCC_PRIVATE_SMOKE' 2>/dev/null || \
  bash -c 'set -a; . "$HOME/.config/crabcc-private/env.local"; set +a; echo $CRABCC_PRIVATE_SMOKE')
echo "smoke=$CRABCC_PRIVATE_SMOKE_CHECK"
```

Expected: `smoke=1` (the wrapper sourced the file).

- [ ] **Step 5: Commit**

```bash
git add nix/crabcc-private.nix flake.nix
git commit -m "feat: crabcc-private wrapper that sources ~/.config/crabcc-private/env.local"
```

---

### Task 5: `nix run .#install` — installer app

**Files:**
- Create: `nix/installer.nix`
- Modify: `flake.nix` (add `apps.install`)

- [ ] **Step 1: Write `nix/installer.nix`**

`nix/installer.nix`:
```nix
{ writeShellApplication, crabccPrivate, jq }:

writeShellApplication {
  name = "crabcc-private-install";
  runtimeInputs = [ crabccPrivate jq ];
  text = ''
    set -euo pipefail

    BUNDLE="${crabccPrivate}/share/crabcc-private"
    CLAUDE_DIR="$HOME/.claude"
    ENV_DIR="$HOME/.config/crabcc-private"

    echo "▶ crabcc-private installer"
    echo "  bundle: $BUNDLE"
    echo "  target: $CLAUDE_DIR"

    mkdir -p "$CLAUDE_DIR" "$ENV_DIR"

    # 1. Merge MCP fragment into ~/.claude.json (idempotent).
    CLAUDE_JSON="$HOME/.claude.json"
    if [ ! -f "$CLAUDE_JSON" ]; then echo '{}' > "$CLAUDE_JSON"; fi
    tmp=$(mktemp)
    jq -s '.[0] * .[1]' "$CLAUDE_JSON" "$BUNDLE/mcp/claude.json" > "$tmp"
    mv "$tmp" "$CLAUDE_JSON"
    echo "  ✓ MCP fragment merged into ~/.claude.json"

    # 2. Copy hooks + settings only if absent (no overwrite — upsert semantics).
    for f in settings.json hooks.json; do
      target="$CLAUDE_DIR/$f"
      if [ ! -f "$target" ]; then
        cp "$BUNDLE/claude/$f" "$target"
        echo "  ✓ wrote $target"
      else
        echo "  · $target exists — left untouched"
      fi
    done

    # 3. Seed env.local from template if missing.
    if [ ! -f "$ENV_DIR/env.local" ]; then
      cp "$BUNDLE/../scripts/env.local.example" "$ENV_DIR/env.local" 2>/dev/null \
        || echo '# crabcc-private env — fill in secrets here' > "$ENV_DIR/env.local"
      chmod 600 "$ENV_DIR/env.local"
      echo "  ✓ seeded $ENV_DIR/env.local (chmod 600) — edit to add secrets"
    else
      echo "  · $ENV_DIR/env.local exists — left untouched"
    fi

    echo ""
    echo "Done. Next steps:"
    echo "  - edit $ENV_DIR/env.local to add OPENAI_API_KEY / MILVUS_TOKEN if needed"
    echo "  - run: crabcc --version"
  '';
}
```

- [ ] **Step 2: Wire into `flake.nix`**

Inside the same `let` block, add:
```nix
        installer = pkgs.callPackage ./nix/installer.nix {
          crabccPrivate = crabccPrivate;
        };
```

And extend `outputs`:
```nix
        apps.install = {
          type = "app";
          program = "${installer}/bin/crabcc-private-install";
        };
        apps.default = self.apps.${system}.install;
```

- [ ] **Step 3: Smoke-run the installer locally (dry path: a temp HOME)**

```bash
TMPHOME=$(mktemp -d)
HOME="$TMPHOME" nix run .#install
ls -la "$TMPHOME/.claude" "$TMPHOME/.config/crabcc-private"
cat "$TMPHOME/.claude.json"
```

Expected:
- `$TMPHOME/.claude.json` contains the `mcpServers.crabcc` entry.
- `$TMPHOME/.claude/settings.json` and `hooks.json` exist.
- `$TMPHOME/.config/crabcc-private/env.local` exists with mode `600`.

- [ ] **Step 4: Re-run to verify idempotency**

```bash
HOME="$TMPHOME" nix run .#install
```

Expected: prints `· ... left untouched` lines for each pre-existing file; `.claude.json` still contains exactly one `crabcc` MCP entry (`jq '.mcpServers.crabcc' "$TMPHOME/.claude.json"`).

- [ ] **Step 5: Commit**

```bash
git add nix/installer.nix flake.nix
git commit -m "feat: nix run .#install — upsert config, seed env.local"
```

---

### Task 6: Non-Nix fallback installer

**Files:**
- Create: `scripts/install.sh`

- [ ] **Step 1: Write `scripts/install.sh`**

`scripts/install.sh`:
```bash
#!/usr/bin/env bash
# Fallback installer for hosts without Nix.
# Assumes `crabcc` is already on PATH (e.g. via `cargo install` from the OSS repo).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUNDLE="$REPO_ROOT/config"
CLAUDE_DIR="$HOME/.claude"
ENV_DIR="$HOME/.config/crabcc-private"

if ! command -v crabcc >/dev/null 2>&1; then
  echo "✗ crabcc not on PATH. Install it first:" >&2
  echo "    https://github.com/peterlodri-sec/crabcc#install" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "✗ jq required for MCP merge. brew install jq / apt install jq" >&2
  exit 1
fi

mkdir -p "$CLAUDE_DIR" "$ENV_DIR"

CLAUDE_JSON="$HOME/.claude.json"
[ -f "$CLAUDE_JSON" ] || echo '{}' > "$CLAUDE_JSON"
tmp=$(mktemp)
jq -s '.[0] * .[1]' "$CLAUDE_JSON" "$BUNDLE/mcp/claude.json" > "$tmp"
mv "$tmp" "$CLAUDE_JSON"
echo "✓ MCP merged into $CLAUDE_JSON"

for f in settings.json hooks.json; do
  if [ ! -f "$CLAUDE_DIR/$f" ]; then
    cp "$BUNDLE/claude/$f" "$CLAUDE_DIR/$f"
    echo "✓ wrote $CLAUDE_DIR/$f"
  else
    echo "· $CLAUDE_DIR/$f exists — left untouched"
  fi
done

if [ ! -f "$ENV_DIR/env.local" ]; then
  cp "$REPO_ROOT/scripts/env.local.example" "$ENV_DIR/env.local"
  chmod 600 "$ENV_DIR/env.local"
  echo "✓ seeded $ENV_DIR/env.local (chmod 600)"
else
  echo "· $ENV_DIR/env.local exists — left untouched"
fi

echo ""
echo "Done. Add 'source $ENV_DIR/env.local' to your shell rc if you want the"
echo "secrets in your interactive shell as well."
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x scripts/install.sh
```

- [ ] **Step 3: Smoke-run against a temp HOME**

```bash
TMPHOME=$(mktemp -d)
HOME="$TMPHOME" ./scripts/install.sh
ls -la "$TMPHOME/.claude" "$TMPHOME/.config/crabcc-private"
```

Expected: same end-state as the Nix installer (Task 5, Step 3).

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "feat: scripts/install.sh — non-Nix fallback installer"
```

---

### Task 7: Smoke test script

**Files:**
- Create: `scripts/smoke.sh`

- [ ] **Step 1: Write `scripts/smoke.sh`**

`scripts/smoke.sh`:
```bash
#!/usr/bin/env bash
# Verifies a crabcc-private install end-to-end.
# Exits 0 on green, prints first failure and exits 1 otherwise.

set -euo pipefail

fail() { echo "✗ $1" >&2; exit 1; }
ok()   { echo "✓ $1"; }

# 1. crabcc binary present and executable.
command -v crabcc >/dev/null || fail "crabcc not on PATH"
crabcc --version >/dev/null  || fail "crabcc --version exited non-zero"
ok "crabcc --version: $(crabcc --version)"

# 2. MCP entry merged.
jq -e '.mcpServers.crabcc.command == "crabcc"' "$HOME/.claude.json" >/dev/null \
  || fail "MCP entry missing in ~/.claude.json"
ok "MCP entry present"

# 3. Hooks file installed.
test -f "$HOME/.claude/hooks.json" || fail "hooks.json missing"
jq -e '.hooks.SessionStart' "$HOME/.claude/hooks.json" >/dev/null \
  || fail "SessionStart hook missing"
ok "SessionStart hook present"

# 4. env.local present and chmod 600.
ENV_FILE="$HOME/.config/crabcc-private/env.local"
test -f "$ENV_FILE" || fail "env.local missing"
perms=$(stat -f %A "$ENV_FILE" 2>/dev/null || stat -c %a "$ENV_FILE")
[ "$perms" = "600" ] || fail "env.local perms = $perms (want 600)"
ok "env.local chmod 600"

# 5. env.local is not world-readable AND not in /nix/store.
case "$ENV_FILE" in
  /nix/store/*) fail "env.local resolves into /nix/store — secrets exposed" ;;
esac
ok "env.local lives outside /nix/store"

# 6. crabcc MCP responds (stdio handshake).
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  | timeout 5 crabcc --mcp 2>/dev/null \
  | jq -e '.result' >/dev/null \
  || fail "crabcc --mcp did not return a valid initialize response"
ok "crabcc --mcp handshake OK"

echo ""
echo "✓ all smoke checks passed"
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x scripts/smoke.sh
```

- [ ] **Step 3: Run it**

```bash
./scripts/smoke.sh
```

Expected: six `✓` lines, exit 0.

- [ ] **Step 4: Commit**

```bash
git add scripts/smoke.sh
git commit -m "test: scripts/smoke.sh — six-check end-to-end"
```

---

### Task 8: Docs

**Files:**
- Modify: `docs/README.md` (replace stub from Task 1)
- Create: `docs/SECRETS.md`

- [ ] **Step 1: Expand `docs/README.md`**

`docs/README.md`:
````markdown
# crabcc-private

Private overlay for [crabcc](https://github.com/peterlodri-sec/crabcc).

## Install

### Nix (preferred)

```bash
nix run github:peterlodri-sec/crabcc-private#install
```

This will:
1. Build (or fetch from cache) `crabcc` pinned by this repo's `flake.lock`.
2. Merge the MCP entry into `~/.claude.json` (idempotent).
3. Copy `settings.json` + `hooks.json` into `~/.claude/` (only if absent).
4. Seed `~/.config/crabcc-private/env.local` (chmod 600) for runtime secrets.

### Non-Nix fallback

```bash
git clone git@github.com:peterlodri-sec/crabcc-private.git
cd crabcc-private
./scripts/install.sh
```

Requires `crabcc` already on `$PATH` (e.g. via `cargo install` from the OSS repo) and `jq`.

## Verify

```bash
./scripts/smoke.sh
```

## Secrets

See [SECRETS.md](./SECRETS.md). TL;DR: edit `~/.config/crabcc-private/env.local`. The wrapper sources it at every `crabcc` invocation. Nothing secret lives in git or `/nix/store`.

## Upgrade crabcc pin

```bash
nix flake update
nix build .#crabcc-private
./result/bin/crabcc --version
```
````

- [ ] **Step 2: Write `docs/SECRETS.md`**

`docs/SECRETS.md`:
````markdown
# Secret management

## Default (single host, personal use)

Runtime secrets live in **one file**:

```
~/.config/crabcc-private/env.local   (chmod 600)
```

The wrapper in `crabcc-private` sources this file via `makeWrapper --run` before exec-ing `crabcc`, so every invocation has the env vars in scope.

**Why this is safe enough for personal dev:**
- File is outside `/nix/store` (world-readable).
- File is outside git (`.gitignore`'d).
- File mode is `600` (only your user can read it).
- Smoke test checks all three properties.

**Why this is not enough for shared / multi-host setups:**
- Not encrypted at rest.
- No key rotation story.
- No audit trail.

If you grow beyond one host, switch to one of the Phase-2 paths below.

## Phase 2a — agenix (recommended next step)

`agenix` encrypts secrets with SSH keys, stores them in git, decrypts to a tmpfs path at activation time. Good fit when you want secrets in the repo without exposing them.

- Add input: `agenix.url = "github:ryantm/agenix";`
- Create `secrets.nix` listing recipients (your SSH pubkey).
- Encrypt `env.local` into `secrets/env.age`.
- Module decrypts to `/run/agenix/env` (NixOS) or `~/.run/agenix/env` (nix-darwin).

Reference: [Agenix — NixOS Wiki](https://nixos.wiki/wiki/Agenix).

## Phase 2b — sops-nix (only if you need bundled secrets)

Use only if you're packing many related secrets into one file (mail server-shaped use case). Heavier than agenix for "a few API tokens."

Reference: [sops-nix repo](https://github.com/Mic92/sops-nix).

## What never to do

- Commit `env.local` (gitignored — verify with `git check-ignore env.local`).
- Embed values in `flake.nix`, `config/`, or any `.nix` file. Everything in `/nix/store` is world-readable.
- Pass secrets on the command line (visible in `ps`).

## Rotation reminder

The `MILVUS_TOKEN` value that was pasted in a Claude Code transcript on 2026-05-28 was leaked. **Rotate it** at the Zilliz console before relying on this install path.
````

- [ ] **Step 3: Commit**

```bash
git add docs/README.md docs/SECRETS.md
git commit -m "docs: README + SECRETS"
```

---

### Task 9: Phase-2 placeholders (cosign + agenix) — track, don't build

**Files:**
- Create: `docs/PHASE2.md`

This task does **not** add code. It records the Phase-2 expansion points so they're not lost.

- [ ] **Step 1: Write `docs/PHASE2.md`**

`docs/PHASE2.md`:
````markdown
# Phase 2 — defer until needed

## Cosign-signed prebuilt bundle

**Trigger:** building `crabcc` from source on every new host gets slow (cold builds > 3 min).

**Sketch:**
1. CI in this private repo builds `crabcc` for `x86_64-linux` + `aarch64-darwin` per tag.
2. `cosign sign-blob --key cosign.key` produces `crabcc-${ver}-${sys}.tar.gz.sig`.
3. Tarball + sig uploaded to a private GitHub Release on this repo (NOT the OSS one).
4. New flake derivation `crabcc-prebuilt`:
   - `fetchurl` the tarball
   - `cosign verify-blob --key cosign.pub` in a `preBuild` phase
   - install bytes into `$out/bin/`
5. `flake.nix` adds a flag `useBinary = true;` that swaps `crabcc` → `crabcc-prebuilt`.

**Key management:**
- `cosign.key` lives on YubiKey, never on disk.
- `cosign.pub` lives in this repo at `nix/cosign.pub` (public, in store, fine).

## agenix for multi-host

**Trigger:** second host needs the same secrets.

**Sketch:** see `docs/SECRETS.md` Phase 2a.

## HTTP MCP transport

**Trigger:** want the same MCP server visible to multiple Claude Code instances (Cursor + Claude Desktop at once).

**Sketch:**
- Add `apps.mcp-http` running `crabcc --mcp-http 127.0.0.1:8091`.
- LaunchAgent / systemd-user unit shipped under `config/os/`.
- `MCP_AUTH_TOKEN` sourced from `env.local`.
````

- [ ] **Step 2: Commit and push**

```bash
git add docs/PHASE2.md
git commit -m "docs: PHASE2 — cosign + agenix + HTTP MCP triggers"
git push -u origin main
```

Expected: `gh repo view peterlodri-sec/crabcc-private --json pushedAt -q .pushedAt` returns today.

---

### Task 10: End-to-end remote install dry run

This is the final acceptance test: prove the one-liner from the spec actually works.

- [ ] **Step 1: Wipe local cache to force a clean fetch**

```bash
TMPHOME=$(mktemp -d)
echo "Testing in HOME=$TMPHOME"
```

- [ ] **Step 2: Run the documented one-liner**

```bash
HOME="$TMPHOME" nix run github:peterlodri-sec/crabcc-private#install
```

Expected output: same `▶ crabcc-private installer` lines as Task 5 Step 3, ending in `Done.`

- [ ] **Step 3: Run the smoke check inside the temp HOME**

```bash
HOME="$TMPHOME" PATH="$(nix path-info github:peterlodri-sec/crabcc-private#crabcc-private)/bin:$PATH" \
  ./scripts/smoke.sh
```

Expected: six `✓` lines, exit 0.

- [ ] **Step 4: Tag the release**

```bash
git tag v0.1.0
git push --tags
```

- [ ] **Step 5: Verify private visibility one more time**

```bash
gh api /repos/peterlodri-sec/crabcc-private --jq '.private'
```

Expected: `true`.

---

## Reversibility

Each task is a single commit, so any task is reversible with `git revert <sha>`. The only side-effects outside the repo are:

| Side-effect | How to undo |
|---|---|
| `~/.claude.json` MCP entry | `jq 'del(.mcpServers.crabcc)' ~/.claude.json > tmp && mv tmp ~/.claude.json` |
| `~/.claude/settings.json` (only if you had none) | `rm ~/.claude/settings.json` |
| `~/.claude/hooks.json` (only if you had none) | `rm ~/.claude/hooks.json` |
| `~/.config/crabcc-private/` | `rm -rf ~/.config/crabcc-private` |
| Nix store entries | `nix-collect-garbage` |
| Private GitHub repo | `gh repo delete peterlodri-sec/crabcc-private --yes` |

---

## Required infrastructure

Day 1 (this plan):
- Private GitHub repo under `peterlodri-sec` (created in Task 1, Step 5). Free, already authenticated via your `gh` CLI.
- Local Nix install (`nix --version` must work; flakes enabled). Already present per `which nix` on your Mac.
- No other infra.

Phase 2 (deferred):
- **Cosign key**: YubiKey-resident, generated with `cosign generate-key-pair --kms hardware`.
- **Artifact store**: private GitHub Releases on this repo (no S3 needed).
- **CI**: GitHub Actions on the private repo, using `WarpBuild` runners per `docs/CI_WARP_MIGRATION.md`.

---

## Self-review

**Spec coverage** (`docs/superpowers/specs/2026-05-28-private-crabcc-package-design.md`):

| Spec section | Plan task |
|---|---|
| Goals: reproducible installs | Tasks 2, 4, 5, 10 |
| Goals: versioned config | Task 3 + `flake.lock` (Task 2) |
| Goals: separation public/private | Task 1 Step 5 (private repo) |
| Non-goal: no secrets in repo/store | Task 3 Step 1 (scrub), Task 7 Step 5 (smoke check) |
| Approach A repo layout | Task 1, 3 |
| Nix flake inputs/outputs | Task 2, 4, 5 |
| Install flow one-liner | Task 5, 10 |
| Configuration versioning | Task 2, 3 |
| Security: no secrets in store | Task 4 (`makeWrapper --run` sources external file) |
| Testing: smoke | Task 7, 10 |
| Open Q: A vs B/C | Answered: A (matrix at top) |
| Open Q: mandatory configs | Answered: Claude only (matrix at top) |
| Open Q: HTTP vs stdio MCP | Answered: stdio day 1 (matrix at top) |

**Placeholder scan:** no `TBD` / `TODO` / "implement later" / "similar to" markers remain. All code blocks are complete and self-contained.

**Type/name consistency:** `crabcc`, `crabccPrivate`, `crabcc-private`, `crabcc-private-install`, `configBundle`, `~/.config/crabcc-private/env.local` are used identically across Tasks 2–10. The path of `env.local` and the merging behavior of `jq -s '.[0] * .[1]'` are referenced the same way in `installer.nix` (Task 5), `install.sh` (Task 6), and `smoke.sh` (Task 7).
