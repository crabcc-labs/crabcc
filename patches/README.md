# patches/

Local patches for upstream repos we depend on but don't own.
Apply with `git apply <patch>` from the target repo root.

| Patch | Target repo | What it does |
|---|---|---|
| `free-claude-code-ollama-api-key.patch` | `Alishahryar1/free-claude-code` | OLLAMA_API_KEY env var support (LiteLLM Bearer auth) + .env cleanup to ollama-only |

## Apply

```bash
cd ~/workspace/bin/free-claude-code
git apply ~/workspace/bin/crabcc/patches/free-claude-code-ollama-api-key.patch
```

## Regenerate

```bash
cd ~/workspace/bin/free-claude-code
git format-patch origin/main..feat/ollama-apple-litellm --stdout \
  > ~/workspace/bin/crabcc/patches/free-claude-code-ollama-api-key.patch
```
