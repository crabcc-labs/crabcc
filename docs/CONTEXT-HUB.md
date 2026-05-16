# LangSmith Context Hub

The LangSmith Context Hub stores versioned snapshots of the agent
context files that downstream runtimes (AgentField, Claude Code,
custom orchestrators) pull at start-up. This keeps AGENTS.md,
CLAUDE.md, the crabcc skill, and the slash-command descriptions
in sync across all environments without embedding them in container
images or prompting stacks.

## Files pushed

| Repo path | Hub name |
|---|---|
| `AGENTS.md` | `AGENTS.md` |
| `CLAUDE.md` | `CLAUDE.md` |
| `skill/crabcc/SKILL.md` | `skill/crabcc/SKILL.md` |
| `commands/crabcc-init.md` | `commands/crabcc-init.md` |
| `commands/crabcc-upgrade.md` | `commands/crabcc-upgrade.md` |

## Promotion flow

```
push to main
  └── contexthub-promote.yml  ->  scripts/contexthub_push.py --env staging
        └── tags commit in Context Hub as: staging

publish release tag (v*.*.*)
  └── contexthub-promote.yml  ->  scripts/contexthub_push.py --env production
        └── tags commit in Context Hub as: production
```

The workflow lives at `.github/workflows/contexthub-promote.yml`. It
runs on the EU tenant (`https://eu.api.smith.langchain.com`).

## Pulling a context locally

Install the SDK once:

```bash
pip install "langsmith>=0.7.35"
```

Then pull by tag:

```python
from langsmith import Client

client = Client()                               # uses LANGSMITH_API_KEY + LANGSMITH_ENDPOINT
agent = client.pull_agent("crabcc:production")  # or "crabcc:staging"

# agent.files is a dict of name -> FileEntry
print(agent.files["AGENTS.md"].content)
```

The identifier syntax is `<name>:<tag>` for a tag, or
`<name>:<commit-sha>` for a pinned commit.

## Local development

1. Export your personal LangSmith API key (EU tenant):

   ```bash
   export LANGSMITH_API_KEY=ls__...
   export LANGSMITH_ENDPOINT=https://eu.api.smith.langchain.com
   ```

2. Dry-run the push (no API call, no langsmith import required):

   ```bash
   python scripts/contexthub_push.py --dry-run
   ```

3. Push to staging from your branch (real API call):

   ```bash
   python scripts/contexthub_push.py --env staging
   ```

## GitHub Actions secret

Add `LANGSMITH_API_KEY` under **Settings > Secrets and variables >
Actions** in the repository. The workflow fails with a clear error
message if the secret is absent. The key is scoped to the EU tenant;
ensure it was generated at `https://eu.smith.langchain.com`.
