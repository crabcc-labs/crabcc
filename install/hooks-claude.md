# Claude Code hooks — `install/hooks-claude.json`

Reference template for paste-into-`~/.claude/settings.json` integration of crabcc with Claude Code. Three hooks ship by default:

1. **`SessionStart` (matcher = `startup`)** — refresh `.crabcc/index.db` if the repo has been indexed; print a hint to stderr otherwise. Fires only on new session starts (not on `resume`, `clear`, or `compact`). It also calls `crabcc shell context`, which is a **no-op unless** the experimental flag is on (`CRABCC_EXP_CTX_INJECT=1`, or the `--exp-ctx-inject` flag): when enabled it emits a SessionStart `hookSpecificOutput.additionalContext` carrying standing reminders (query context7 for current docs, prefer `crabcc lookup` over grep, open GitHub issues for discoveries). SessionStart is the documented low-cost injection point (once per session, not per turn/tool). Override the reminder text per-repo with `.crabcc/ctx-inject.md`. The index-refresh output stays on stderr/`/dev/null` so the hook's stdout is clean JSON-or-empty (plain text and JSON are mutually exclusive per hook).

2. **`PreToolUse` (matcher = `Bash`) — command rewrite** — when the agent is about to shell out, the hook (a) records the command for the loop detector (`crabcc shell record`) and (b) plans a *safe rewrite* (`crabcc shell rewrite`). When the command is a `grep`/`find` the rewriter can prove equivalent, it emits a `hookSpecificOutput.updatedInput` envelope so the cheaper modern form runs transparently in its place: `grep -rn IDENT` → `crabcc lookup refs IDENT` (only when `IDENT` is an indexed symbol), `grep -rn P` → `rg -n P`, `find PATH -name GLOB` → `rg --files -g GLOB PATH`, and `cat <src>` → `crabcc read` (byte-exact first read, outline stub on re-read). The rewritten command's output is prefixed with a `## crabcc-rewrite […]` header (rule + estimated tokens saved + caveats), every rewrite emits a `tracing` event (`target = crabcc::shell::rewrite`) and a `crabcc track` ledger row, and anything the rewriter can't prove safe (pipes, perl regex, unknown flags, `-exec`, regex that differs between grep-BRE and ripgrep) passes through untouched. Non-blocking (`exit 0` always). Set `CRABCC_NO_REWRITE=1` to disable rewriting and fall back to record-only.

3. **`PreToolUse` (matcher = `Bash`) — gh/GitHub CLI** — intercepts `gh pr/issue/run/release/workflow/api` calls and emits a hint with the equivalent `mcp__github__*` tool. Specifically flags the dispatch+poll chain (`gh workflow run` + `sleep` + `gh run list`) as a known anti-pattern with a race condition on run ID, pointing to `mcp__github__actions_run_trigger + actions_list/actions_get` instead.

4. **`PreToolUse` (matcher = `Read`)** — when the agent is about to `Read` a file, the hook branches on type:
   - **Source/code file** → warms the `crabcc read` session cache (so the next re-read is a cheap outline stub) and prints a one-line hint to stderr. The original `Read` proceeds unchanged.
   - **Image (`png`/`jpg`/`jpeg`)** → calls `crabcc media downscale`, which bounds an oversized image to ~1568 px on the long edge (Anthropic's effective vision resolution) and caches the copy under `~/.crabcc/media-cache/`. Vision tokens scale with image **area**, so a 4000×3000 screenshot drops from ~16k to ~2.5k vision tokens (−85%) with no resolvable detail lost. The hook emits an `updatedInput.file_path` pointing `Read` at the bounded copy and records the saved tokens to the `crabcc track` ledger (op `media`). Lossless-on-failure: a non-image, decode error, already-small image, or `CRABCC_NO_MEDIA=1` all leave the original read untouched. Video/audio are *not* handled — Claude Code does not tokenize them, so there is nothing to reduce.

## Optional compaction chain: crabcc → RTK → Morph

`crabcc shell rewrite` builds a **gated pipeline**, not a single rewrite, so the engine rewrite composes with two optional stdin-filter stages (no hook JSON change — it's all inside `updatedInput`):

```
<engine rewrite>  | rtk pipe <filter>     | crabcc morph compact --query Q
grep→rg/lookup      CRABCC_RTK_PIPE set      MORPH_API_KEY set
                    + rtk on PATH            (large compact-worthy outputs)
```

- **Why a pipeline, not three hooks** — multiple PreToolUse hooks each emitting `updatedInput` is undefined; chaining as pipe stages inside one rewritten command is deterministic. Each stage is a passthrough filter when disabled, so the chain never loses output.
- **[Morph](https://morphllm.com) Compact** (`crabcc morph compact`, `POST /v1/compact`) — query-conditioned, byte-verbatim 50-70% shrink of large output (`cat`/`gh`/`git`/`rg` dumps). **Off unless `MORPH_API_KEY` is set** (privacy gate — code never leaves the machine otherwise). Degrades to full passthrough on no-key / network / parse error. PostToolUse *cannot* replace tool output (it only appends), so compaction must run in the command's own pipeline — hence PreToolUse.
- **Morph Fast Apply** (`crabcc morph apply --file F --update '<lazy edit>'`, `morph-v3-fast`) — fast lazy-edit merge, delivered as a subcommand (not hooked onto the exact-match Edit tool, which it would interfere with).
- **Latency** — the hook adds ~18 ms/call (almost all crabcc process spawn; the rewrite logic is sub-ms), +2 ms for grep/find candidates (which open the dev-debug ledger). Passthrough commands do zero SQLite work. Morph adds a network round-trip only on large compact-worthy outputs.
- **Caching** — the measure/learn ledger (`~/.crabcc/_internal.db`, `rewrite_log`/`rewrite_suppress`, pruned ~2 MB) is separate from the symbol index, the `read` cache, and Claude Code's prompt cache — it touches none of them. Suppression is bounded to a recent 7-day window; measurement matches commands 1:1 by exact string.

## Verified against

- [Claude Code hooks reference](https://code.claude.com/docs/en/hooks) — last cross-checked 2026-04-30.
- [Morph Compact](https://docs.morphllm.com/sdk/components/compact) + [Fast Apply](https://docs.morphllm.com/quickstart) API — cross-checked 2026-06-04.

## Audit deltas (v2.5.x sweep, issue #29)

| Field | Before | After | Reason |
|---|---|---|---|
| Hook stdin | `echo "$CLAUDE_HOOK_INPUT" \| grep …` | `jq -r '.tool_input.command' \| grep …` | `$CLAUDE_HOOK_INPUT` is **not** an env var — Claude Code pipes the hook payload as JSON over stdin. The old shape silently no-op'd because `$CLAUDE_HOOK_INPUT` was always empty. |
| `SessionStart.matcher` | (absent — fires on every session start type) | `"startup"` | Without an explicit matcher the hook also fires on `resume`, `clear`, and `compact` events, which can re-trigger an index refresh mid-session (slow + noisy). `startup` restricts it to the new-session path where re-indexing is the right move. |
| `PreToolUse` regex | `'rg X\|grep -X X\|find … -name'` | `'(^\| )(rg\|grep( -X)?)\s+IDENT\|(^\| )find\s+[^\|]*-name\b'` | Anchors the verb to a word boundary (`^` or whitespace), so the hint won't fire when `rg` / `grep` / `find` appear *inside* a path or another word (`./scripts/grep-helper`, `tools/find-stuff`). Also tolerates short/long grep flag forms (`grep -i`, `grep -nE`). |
| `SessionStart` stderr | unset (printed to stdout) | redirected to stderr (`>&2`) | Hooks' stdout is captured into the transcript verbatim; stderr surfaces in the user-facing terminal log. The "no index" hint is operator-facing, not agent-facing, so stderr is the right channel. |

## Schema notes (no change required)

- `hooks[*].type: "command"` — current shape, no v2.x rename.
- `PreToolUse.matcher: "Bash"` — exact-match on tool name (case-sensitive). Subcommand-specific filtering uses the **permission-rule** syntax (`Bash(git *)`), which lives under `permissions.allow` / `.deny`, not under hooks. The two systems are independent.
- Exit codes:
  - `0` — allow the tool call (this is what we use; we're advisory only).
  - `2` — block the tool call; stderr is shown to Claude as an error. Reserved for stricter hooks (e.g. block destructive `rm -rf`).
  - any other code — non-blocking error; stderr surfaces in transcript only. Avoid; either advise (`0`) or block (`2`).
- `hookSpecificOutput` JSON-on-stdout shape exists for richer control (`permissionDecision: "allow" | "deny" | "ask" | "defer"`, `updatedInput`, `additionalContext`). We don't use it because the hint hook is purely advisory; promote when a future hook needs to mutate the input or branch on a decision.

## Smoke test

After `crabcc install-claude` (or after pasting `install/hooks-claude.json` into `~/.claude/settings.json`):

```bash
# 1. SessionStart hook fires on new sessions in an indexed repo:
cd /path/to/an/indexed/repo
claude --print "what is in this repo?"   # `.crabcc/index.db` mtime should bump

# 2. PreToolUse hint fires when the agent shells out to grep/rg/find:
claude --print "use bash to find references to UserId via grep"
# stderr should contain: "hint: try `crabcc sym/refs/callers` …"

# 3. Hint does NOT fire on bash that doesn't match the regex:
claude --print "list this directory with ls"
# no hint expected
```

Manual JSON-shape sanity check (no Claude session needed):

```bash
# Pipe a synthetic PreToolUse payload through the hook command:
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rg UserId src/"}}' | \
  jq -r '.tool_input.command // ""' | \
  grep -qE '(^| )(rg|grep( -[a-zA-Z]+)?)\s+[A-Za-z_][A-Za-z0-9_]+|(^| )find\s+[^|]*-name\b' && \
  echo "would print hint"

# negative case:
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls -la"}}' | \
  jq -r '.tool_input.command // ""' | \
  grep -qE '(^| )(rg|grep( -[a-zA-Z]+)?)\s+[A-Za-z_][A-Za-z0-9_]+|(^| )find\s+[^|]*-name\b' && \
  echo "would print hint" || echo "no hint (correct)"
```

## Project-local vs global precedence

Per the [hooks reference](https://code.claude.com/docs/en/hooks#configuration-precedence), Claude Code reads hooks from (in increasing precedence):

1. `~/.claude/settings.json`
2. `<repo>/.claude/settings.json`
3. `<repo>/.claude/settings.local.json`

Our template ships as the global-level snippet (`~/.claude/settings.json` reach). To make it project-only, paste the same JSON into `<repo>/.claude/settings.json` instead — the `SessionStart` refresh will then only run when Claude opens *this* repo. Useful when you have multiple crabcc-indexed repos and want the auto-refresh to be opt-in per project.

## Smoke test — gh hook

After installing hooks, test the gh interception hint:

```bash
# Dispatch+poll chain should flag the anti-pattern:
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh workflow run foo.yml && sleep 5 && gh run list --limit 1"}}' | \
  bash -c 'input=$(cat); cmd=$(echo "$input" | jq -r '"'"'.tool_input.command // ""'"'"'); chain=0; echo "$cmd" | grep -q "gh run list" && echo "$cmd" | grep -q "sleep " && chain=1; [ "$chain" = "1" ] && echo "hint(gh-chain) would fire"'
# → hint(gh-chain) would fire

# gh pr should suggest pull_request_read:
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh pr view 123 --json title"}}' | \
  bash -c 'input=$(cat); cmd=$(echo "$input" | jq -r '"'"'.tool_input.command // ""'"'"'); echo "$cmd" | grep -q "gh pr " && echo "hint(gh: pr) would fire"'
# → hint(gh: pr) would fire
```

## Future hooks (not shipped, considered)

- **`Stop` hook** that flushes the in-process token-saved counter to `~/.crabcc/usage.log`. Currently the CLI writes the log per-call; a `Stop` hook would pre-aggregate.
- **`PreToolUse` block** on `rm -rf .crabcc/` (with exit code 2) — paired with the existing permission rule. Defer until profiling shows users actually try this.
- **`SubagentStop` / `Notification`** hooks — no concrete crabcc use case; track separately if proposed.
