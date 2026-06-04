# Claude Code hooks — `install/hooks-claude.json`

Reference template for paste-into-`~/.claude/settings.json` integration of crabcc with Claude Code. Three hooks ship by default:

1. **`SessionStart` (matcher = `startup`)** — refresh `.crabcc/index.db` if the repo has been indexed; print a hint to stderr otherwise. Fires only on new session starts (not on `resume`, `clear`, or `compact`).

2. **`PreToolUse` (matcher = `Bash`) — symbol search** — when the agent is about to shell out, peek at the bash command and, if it looks like a symbol lookup via `rg`/`grep`/`find -name`, nudge it toward `crabcc sym/refs/callers` via stderr. Non-blocking (`exit 0` always); the agent gets the hint as transcript context.

3. **`PreToolUse` (matcher = `Bash`) — gh/GitHub CLI** — intercepts `gh pr/issue/run/release/workflow/api` calls and emits a hint with the equivalent `mcp__github__*` tool. Specifically flags the dispatch+poll chain (`gh workflow run` + `sleep` + `gh run list`) as a known anti-pattern with a race condition on run ID, pointing to `mcp__github__actions_run_trigger + actions_list/actions_get` instead.

## Verified against

- [Claude Code hooks reference](https://code.claude.com/docs/en/hooks) — last cross-checked 2026-04-30.

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
