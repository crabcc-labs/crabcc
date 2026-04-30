# crabcc integrations

> Status-line + IDE wiring for the `crabcc info --status-line` /
> `--is-repo` surface added in issue
> [#43](https://github.com/peterlodri-sec/crabcc/issues/43).

The shape of the data is the same in every consumer:

```text
crabcc 87.2k · idx 12s · mem 1.4k · 4 tools
```

| Position | Segment | Source | Kept short because |
|---|---|---|---|
| 1 | `87.2k` | `crabcc_core::track::report().all_time.saved_tokens` | Single file read on `.crabcc/track.json`. |
| 2 | `idx 12s` | `mtime(.crabcc/index.db)` vs wall clock | One `stat()` syscall. |
| 3 | `mem 1.4k` | `Palace::open(root).count()` | Cached registry → one `SELECT COUNT(*)` over `drawers`. |
| 4 | `4 tools` | `tail`-parse most recent `~/.claude/projects/<root>/sessions/*.jsonl` | Substring count of `"type":"tool_use"` — avoids JSON parse cost. |

Position is meaning: tokens saved → index age → memory drawers →
Claude Code tool calls. No qualifier text — the order is the schema.

Segments degrade gracefully: a missing source is dropped silently,
not errored. Starship hides the whole module when `crabcc info
--is-repo` exits non-zero, so "not in a crabcc repo" renders nothing.

## Starship

```toml
# ~/.config/starship.toml
[custom.crabcc]
command = "crabcc info --status-line"
when    = "crabcc info --is-repo"
format  = "[$output]($style) "
style   = "#d97757"
shell   = ["sh", "--noprofile", "--norc"]
```

`when = ...` runs on every prompt render and gates whether the module
displays. `crabcc info --is-repo` is a pure exit-code check — no
stdout — so it's the right shape for the gate. The `[…]($style)`
wrapper preserves whatever color the user theme selects.

## tmux status-right

```tmux
# ~/.tmux.conf
set -g status-interval 5
set -g status-right '#(crabcc info --status-line 2>/dev/null) | #(date +%H:%M)'
```

`status-interval 5` matches the typical render cadence — `crabcc
info --status-line` is well under that budget. Pipe through
`2>/dev/null` so a transient error (e.g. mid-`crabcc index`)
doesn't leak a stack trace into the bar.

## VS Code (status bar via the Tasks API or a small extension)

If you don't want to write an extension, a one-line custom-task in
`.vscode/tasks.json` is enough:

```json
{
    "version": "2.0.0",
    "tasks": [
        {
            "label": "crabcc status",
            "type": "shell",
            "command": "crabcc info --status-line --json",
            "problemMatcher": [],
            "presentation": { "reveal": "never", "panel": "dedicated" }
        }
    ]
}
```

JSON output is ergonomic for any extension that wants to surface
specific fields (e.g., a dedicated indicator for `index_age` going
red after 1h). The shape is:

```json
{
  "saved_tokens": "1.5M",
  "index_age": "12s",
  "drawer_count": "1.4k",
  "cc_tools": 4,
  "root": "/Users/.../my-repo"
}
```

## Render budget

The `--status-line` path was profiled at p95 ~10–20ms on M-series
Mac after the binary cache warms. First-shot cold can hit 200–300ms
because `dyld` has to map fresh — Starship's first prompt always
takes a beat regardless. Subsequent renders fit comfortably inside
Starship's 50ms render budget.

## Demo gif

The end-to-end loop:

1. Open Claude Code in this repo.
2. Watch the prompt module update as tool calls fire — `cc N tools`
   ticks up.
3. Run `crabcc refresh` in another pane; `idx Ns` resets to `1s`,
   then climbs.
4. `crabcc memory remember 'doc:1' 'fox jumps'`; `mem N drawers`
   bumps.

Recording lives at `assets/status-line-demo.gif` (TODO — capture
once the 50ms-budget acceptance check runs on Linux x86_64 too).

## Roadmap

- **Now (this PR)**: stdio-only — the four segments above.
- **Next** (separate issue): MCP server gains a streaming transport
  (WebSocket / SSE). The status-line could subscribe to push updates
  rather than polling on each prompt render — would drive `idx_age` /
  `cc_tools` to true real-time without a daemon. Out of scope for
  the status-line PR; see the WebSocket / SSE proposal in the same
  issue review thread.
