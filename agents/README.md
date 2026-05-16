# Agent Manifests

TOML files in this directory define every configurable knob for an agent:
model, system prompt, tool allowlist, timeout, and retry count.

## Versioning

**The git SHA of the TOML file is the version.** There is no `version` field
to forget to bump. Any change to the file — model swap, prompt tweak, timeout
adjustment — produces a new SHA automatically.

At dispatch time `tools/orchestrator/resolve-manifest.sh` derives the version:

```
git rev-parse HEAD:agents/<name>.toml
```

That SHA is stored in every queue row and every output stamp, so you can always
trace a result back to the exact config that produced it.

## Stamp format

Every agent output prepends a one-line stamp:

```
<!-- agent: swe-build | sha: abc1234 | model: deepseek-v4-pro | 2026-05-16T20:45Z -->
```

The stamp appears as:
- The first line of any PR comment the agent posts.
- A `>> $GITHUB_STEP_SUMMARY` line in GitHub Actions.
- The `manifest_sha` column in the `agent_tasks` queue.

## Manifest format

```toml
[agent]
name        = "swe-build"
description = "..."
model       = "openrouter/..."

[agent.prompt]
file = "agents/prompts/<name>.md"   # path relative to repo root

[agent.tools]
allowlist = ["Read", "Write", "Edit", "Bash", "Grep", "Glob"]

[agent.limits]
timeout_minutes = 30
max_retries     = 2
```

Required fields: `agent.name`, `agent.model`, `agent.prompt.file`.

## Adding a new agent

1. Copy an existing TOML and edit it:
   ```
   cp agents/swe-build.toml agents/my-agent.toml
   ```
2. Create its system prompt at `agents/prompts/my-agent.md`.
3. Commit both files. The commit SHA becomes the initial version.
4. Test the resolver: `tools/orchestrator/resolve-manifest.sh my-agent`

No registration step needed. `resolve-manifest.sh` finds any `agents/<name>.toml`
by name at dispatch time.

## Agents in this directory

| File | Model | Purpose |
|---|---|---|
| `swe-build.toml` | deepseek-v4-pro | Architecture, build tasks, correctness work (30 min) |
| `swe-fast.toml` | deepseek-v4-flash | Lint fixes, small focused patches (10 min) |
