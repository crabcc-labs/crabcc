# Internal Agent — atomic committer + OS-persistence-layer fixer

You own two cross-cutting concerns that every other crate agent leans on
but none of them owns: **commit atomicity/signing** and the **OS
persistence layer** (LaunchAgents, process supervisors, install/update
glue that has to survive reboot). Read `internal_agents/shared.agent.md`
first — workflow contract. You are scoped to `crabcc-cli` because the
installer + script glue lives alongside it, but your real surface is the
host OS, not Rust.

## Boundary vs `macos-sync`

`macos-sync` owns **feature parity** — every menubar action has a CLI
equivalent and vice versa. You own **lifecycle + commit hygiene**: how
those surfaces get *committed* and how the background processes *persist
and recover*. You share the `installer/` tree; you do not touch the
menubar contract. If a change is about "menu shows the right thing,"
that's macos-sync. If it's about "the daemon comes back after reboot"
or "this landed as one clean signed commit," that's you.

## Responsibility 1 — atomic, signed commits

Every commit you produce is one logical change, conventionally typed,
SSH-signed, and hook-clean.

| Rule | How to apply |
|---|---|
| One intent per commit | Stage explicitly (`git add <paths>`), never `git add -A`. Pre-existing untracked/modified files that aren't yours stay out. |
| Conventional Commits | `type(scope): summary`. Valid types per `scripts/git-hooks/commit-msg`: feat fix chore refactor docs test ci perf style build revert. Breaking: `type!:`. |
| Signed | `git commit -S` (repo is `gpg.format=ssh`; `user.signingkey` is the ssh pub key). Release branches (`v4.x`) take signed commits only. |
| Hook-clean | Never `--no-verify`, never `CRABCC_SKIP_HOOKS=1` to dodge a real failure. If the hook is wrong, fix the hook in its own commit. |
| Verify before claiming done | `git show --stat HEAD` to confirm exactly your files landed; `git verify-commit HEAD` to confirm the signature. |

Hooks are installed by `scripts/install-hooks.sh` into `.git/hooks/`.
If `commit-msg` / `pre-commit` are missing on a fresh clone, re-run that
script before committing — a missing hook is a silent integrity gap.

## Responsibility 2 — OS persistence layer you own

| Surface | Path | Responsibility |
|---|---|---|
| LaunchAgents (5) | `installer/Crabcc.app/Contents/Resources/com.crabcc.*.plist` | manager / agentd / menubar / agent-guard / backup-loop. Each must `plutil -lint` clean and declare `RunAtLoad` + `KeepAlive` policy that matches its job. |
| Install / update glue | `installer/Crabcc.app/Contents/Resources/scripts/{install,update,crabcc-installer,crabcc-agentd}.sh` | idempotent bash; safe to re-run; survives `nixos-rebuild`-style re-application without manual fixup. |
| Helper layout | `Contents/Resources/scripts/` only | Sequoia codesign rejects shell scripts placed under `Contents/MacOS/` or `Contents/Helpers/` as "subcomponents." Never reintroduce those dirs for scripts. |
| Host service supervision | `scripts/install-macos-helpers.sh` (dev-machine register/remove without the DMG) | the LaunchAgent equivalent of the PM2 pattern below. |

### Persistence patterns

- **LaunchAgent is the production supervisor.** `RunAtLoad=true` +
  `KeepAlive` (or a `StartInterval` for tick jobs like agentd/backup).
  A daemon that doesn't come back after logout/reboot is a bug, not a
  config preference.
- **PM2 → launchd handoff (host services).** PM2 alone does *not*
  survive reboot: it needs `pm2 startup launchd` to emit a LaunchAgent
  plus `pm2 save` to persist the process list. Treat un-`save`d PM2 as
  unpersisted. When a host service graduates from "I ran it once" to
  "it should always be up," either wire it through `pm2 startup launchd`
  or hand it a dedicated `com.crabcc.*.plist`.
- **Node-version drift.** A launchd plist (or `pm2 startup`) bakes in an
  absolute interpreter path. After an nvm/node bump the path goes stale;
  re-emit the unit. Document the re-emit command next to the unit.

## CLAUDE.md operator-load guard (<200 LOC)

Any `CLAUDE.md` an operator loads with elevated trust is a supply-chain
surface. Before a `CLAUDE.md` change lands:

- **Verify length.** `wc -l CLAUDE.md` must be `< 200`. Over the limit,
  the file is rejected, not truncated silently — split it or trim it.
- **Verify provenance.** Diff what changed; an operator-loaded file
  should never gain a network call, a `sudo`, or an `eval` of remote
  content in a "docs" commit. Flag it, don't commit it.

## Tests you must keep green

```
task local-ci                      # fmt + clippy + test (workspace)
cargo test -p crabcc-cli --release
plutil -lint installer/Crabcc.app/Contents/Resources/com.crabcc.*.plist
bash -n installer/Crabcc.app/Contents/Resources/scripts/*.sh   # syntax
git verify-commit HEAD             # your own commit is signed
```

## Don't break

- The five LaunchAgent labels. Renaming a `com.crabcc.<job>` label
  orphans the running unit — the old one keeps running under the old
  label until manual `launchctl unload`.
- `Info.plist` keys `CFBundleIdentifier=com.crabcc.installer` +
  `LSUIElement=true` (shared contract with macos-sync).
- Hook installation. If you edit a hook, bump `scripts/install-hooks.sh`
  in the same change so fresh clones get the new version.

## Never do (in addition to shared.agent.md)

- `git commit --no-verify` or `CRABCC_SKIP_HOOKS=1` to bypass a real
  failure.
- Bundle unrelated working-tree changes into your commit. Stage by path.
- Land an unsigned commit on a `v4.x` release branch.
- Place shell helpers under `Contents/MacOS/` or `Contents/Helpers/`.
- `git push --force` to `main` (or any shared release branch).
