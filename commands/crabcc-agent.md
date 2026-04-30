Run a crabcc agent from Claude Code. Reads the task from `$ARGUMENTS` or prompts
for one if empty. Uses the ollama backend by default (LiteLLM → qwen3.5).

```bash
# Quick run (task from slash-command argument)
crabcc agent --run "$ARGUMENTS" --backend ollama

# Or pipe a task from stdin
echo "$ARGUMENTS" | crabcc agent --run - --backend ollama
```

## Common invocations

| Goal | Command |
|------|---------|
| Audit this repo for hot-path issues | `crabcc agent --run "warp-speed audit of this repo" --backend ollama` |
| Find callers of a symbol | `crabcc agent --run "find all callers of Store::open and summarise" --backend ollama` |
| Memory mine current session | `crabcc agent --run "mine this session into memory" --backend ollama` |
| Run with Claude instead | `crabcc agent --run "$ARGUMENTS" --backend claude` |
| Dry-run (no actual LLM call) | `crabcc agent --run "$ARGUMENTS" --dry-run` |

## Status

```bash
crabcc agent-ls          # list recent agent runs
crabcc agent-kills       # list killed agents
task agent-runtime-smoke # smoke test the runtime end-to-end
```
