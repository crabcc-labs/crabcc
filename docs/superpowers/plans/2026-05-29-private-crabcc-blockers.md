# Private crabcc — Reported Blockers & Operator Decisions

Source: subagent reports + T1/T2 execution of `docs/superpowers/plans/2026-05-28-private-crabcc-flake-implementation.md`.
**Annotated via Plannotator at 2026-05-29 — decisions captured inline below ("⟶ DECIDED" markers).**

---

## B-1. `crabcc-labs/crabcc` is **private** — flake fetch strategy must change

**Reported by:** Task 2 implementer subagent (`agentId: a4085ea4c1f3542d8`, status `DONE_WITH_CONCERNS`).

**Symptom:** `fetchFromGitHub { owner = "peterlodri-sec"; repo = "crabcc"; rev = "..."; }` returns HTTP 404 because the repo moved to `crabcc-labs/crabcc` and is private. Nix's `github:` URL scheme uses unauthenticated `codeload.github.com`.

**Workaround applied (not portable):** the subagent used `builtins.fetchGit { url = "/Users/lodripeter/workspace/peterlodri-sec/crabcc"; rev = "fc6edf3e..."; shallow = true; }` against a local clone. Build green: `crabcc 4.0.0` from local commit. Flake will fail on any other machine.

**Decision needed:** pick one fetch strategy (see runbook §3 in `~/.local/share/nix-runbooks/src/01-disaster-recovery.md`).

| Option | DX | Security | Effort |
|---|---|---|---|
| **A. fine-grained PAT in `~/.config/nix/nix.conf`** (recommended) | github: URLs just work | scoped to one repo, chmod 600, 90-day expiry forces rotation | 3 min |
| B. `git+ssh://git@github.com/crabcc-labs/crabcc?ref=v4.0.0` | needs ssh-agent dance | excellent, no token-in-file | 5 min |
| C. keep `builtins.fetchGit` local clone | terrible (non-portable) | fine | 0 |
| D. vendored tarball, hash committed | bad (manual refresh) | fine | high |
| E. cosign-signed private GitHub Release | best for consumers, worst for maintainer | excellent | Phase 2 |

**Default if no annotation:** A.

**⟶ DECIDED (2026-05-29):** Option A — fine-grained PAT applied. PAT added to `~/.config/nix/nix.conf` (chmod 600 + `sudo chown $(whoami)`). Confirmed by operator.

---

## B-2. `rustPlatform.buildRustPackage` → **HTTP 403** from `crates.io`

**Reported by:** Task 2 implementer subagent.

**Symptom:** `buildRustPackage` with `cargoLock.lockFile` downloads each crate via `fetchurl`, which doesn't send the `cargo/` user-agent. `crates.io` has blocked unauthenticated `fetchurl` requests since late 2024.

**Workaround applied:** switched from `rustPlatform.buildRustPackage` → `crane.lib.buildPackage`. `crane`'s `buildDepsOnly` runs `cargo fetch --locked` inside the sandbox with the right UA.

**Decision needed:** accept the crane dependency permanently or pin the workaround differently?

**Default if no annotation:** accept — `crane` is the de-facto Nix-Rust standard in 2026; reverting to `buildRustPackage` requires a `crates.io` policy change we don't control.

**⟶ DECIDED:** crane accepted permanently. "defacto."

---

## B-3. `apps/crabcc-telegram/Cargo.toml` uses **TOML 1.1** — breaks `builtins.fromTOML`

**Reported by:** Task 2 implementer subagent.

**Symptom:** Nix's `builtins.fromTOML` parses TOML 1.0 only. `apps/crabcc-telegram/Cargo.toml` uses TOML 1.1 multi-line inline tables. Crane's source-tree scan fails on eval.

**Workaround applied:** `apps/crabcc-telegram` is **not** a workspace member, so the subagent excluded it from crane's source scan. Build output unchanged.

**Decision needed:** acceptable as a long-term workaround, or open an upstream issue to rewrite `apps/crabcc-telegram/Cargo.toml` in TOML 1.0?

**Default if no annotation:** accept exclude.

**⟶ DECIDED:** accept exclude — `crabcc-telegram` is being deprecated/archived from 2026-05-30. Workaround becomes moot by upstream removal.

---

## B-4. Manual TODOs (operator-only, blocks downstream tasks)

Surfaced across T1/T2 + earlier nix-runbook design:

### B-4a. Enable Touch ID for `sudo` on this Mac
- **Why:** `~/.local/bin/nix-do` falls back to TTY-only protection without `pam_tid.so`. Verified at 2026-05-28T22:20:27Z by the harness self-test.
- **Correct action (macOS BSD sed needs a real newline after `1i\`):**
  ```bash
  sudo sed -i '' '1i\
  auth       sufficient     pam_tid.so
  ' /etc/pam.d/sudo
  ```
  Single-line form fails with `sed: 1: "1i\auth ...": extra characters after \ at the end of i command`.
- **Time:** 30 s.

**⟶ DECIDED:** done by operator. Runbook §`04-operator-harness.md` corrected with the multi-line sed form.

### B-4b. Rotate leaked `MILVUS_TOKEN`
- **Why:** value beginning `c96bd1b8…` was pasted in a Claude Code transcript on 2026-05-28. Treat as public.
- **Action:** Zilliz console → revoke → mint new → write to `~/.config/crabcc-private/env.local` (chmod 600).
- **Time:** 2 min.

**⟶ DECIDED:** **ignored** by operator. No rotation planned.

### B-4c. Pick cloud-VM provider for remote builder (or "use existing Hetzner self-hosted runner")
- **Why:** `02-build-targeting.md` proposes a `nix-do launch-dev-vm` orchestrator; need a provider before implementing.
- **Options:** Hetzner Cloud / Fly machines / AWS spot / reuse existing Hetzner self-hosted runner.

**⟶ DECIDED — hard operator-only boundaries:**
- **Hetzner self-hosted GitHub runners are untouchable by any agent / script in this codebase.** Operator-keyboard-only. Reusing them as Nix remote builders is **off the table**.
- **`hcloud server create` and `hcloud server delete` are operator-keyboard-only.** No automation, no `nix-do` invocation, no hook. Enforced by `bin/nix-do` `HARD_DENY_PATTERNS`.

→ The `nix-do-launch-vm` orchestrator I was planning is **not implementable as automation**. The runbook can document the manual sequence, but the orchestration script idea is reframed as a documentation-only artefact ("here are the four commands you type by hand, in this order").

### B-4d. (Optional) Apply fine-grained PAT chosen in B-1
- **Depends on:** B-1 decision.

---

## B-5. Operator-approved follow-ups

- **Brave Search API key migration** — was in `~/.claude/settings.json:322` plaintext. Operator approved inline fix (2026-05-29 plannotator annotation).
  **⟶ DONE:** key relocated to `~/.zshenv` (chmod 600). `settings.json` now invokes the brave-search MCP via `bash -c` that fails-fast if `BRAVE_API_KEY` is unset, with a pointer to `~/.zshenv`. Backup at `~/.claude/settings.json.bak.20260529061400`. Recommend rotating the Brave key at some point — it was in plaintext in a tracked-style location for a while.

## B-7. Residual operator TODO (post-T10, optional)

T10's acceptance test confirmed the documented `nix run github:peterlodri-sec/crabcc-private#install` one-liner **404s**: the fine-grained PAT in `~/.config/nix/nix.conf` is scoped to `crabcc-labs/crabcc` only, not to the overlay repo `peterlodri-sec/crabcc-private` itself.

**Status:** not blocking. The install works today via:
- local path: `nix run /path/to/crabcc-private#install`
- SSH: `nix run "git+ssh://git@github.com/peterlodri-sec/crabcc-private?ref=main#install"` (uses your existing GitHub SSH access)

README now documents both. **To make the plain `github:` one-liner work on other machines**, broaden the PAT (or add a second `access-tokens` entry) so it also grants read on `peterlodri-sec/crabcc-private`. Operator action, ~1 min. Optional.

**Also reserved for operator (per harness):** the final human acceptance run is yours to keystroke:
```
nix-do run "git+ssh://git@github.com/peterlodri-sec/crabcc-private?ref=main#install"
```

---

## B-6. State at hand-off

- **crabcc-private repo**: live at <https://github.com/peterlodri-sec/crabcc-private> (PRIVATE). Two commits:
  - `b63b7df3` — T1 scaffold (gitignore + stub README + directory tree)
  - `7a4b9cc` — T1 follow-up: gitignore catch-all `env.local` at any depth (added after the T1 quality reviewer flagged the gap)
- All work for T2 onwards has been **paused**, not abandoned. T2's local subagent produced a working `crabcc 4.0.0` build via `crane` + `builtins.fetchGit` against the local clone (commit `57c7353`). With B-1 now resolved, T2 can be redone with the proper `github:crabcc-labs/crabcc/v4.0.0` fetch.

---

## What unblocks what

```
B-1 decision  ──► T2-T10 of the crabcc-private plan resume
B-4a (Touch ID) ──► nix-do harness gains its full protection layer
B-4b (token)   ──► safe to use claude-context MCP in this session
B-4c (provider)──► I can build bin/nix-do-launch-vm (or nix-do-add-builder)
```

---

## How to respond

Open this file with `plannotator annotate` and:

1. For B-1: highlight option A/B/C/D/E and click Accept on the chosen row.
2. For B-2 / B-3: Accept the default or annotate "revise" with the alternative path.
3. For B-4a–c: Accept (commits you to the action) or Reject (defers indefinitely).
4. Anything you leave un-annotated reverts to the listed default.

The file lives in `docs/superpowers/plans/` and the plannotator PostToolUse hook should have already kicked off an async annotation pass on save.
