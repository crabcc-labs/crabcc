# GitHub MCP — local setup guide

How to add a `GITHUB_PERSONAL_ACCESS_TOKEN` and reload the GitHub MCP server
inside Claude Code, with end-to-end verification.

There are **two** GitHub MCP servers we care about:

| Name                     | Transport       | Where it runs                      |
| ------------------------ | --------------- | ---------------------------------- |
| `plugin:github:github`   | Streamable HTTP | hosted at `api.githubcopilot.com`  |
| `github-local`           | stdio           | local Docker (`ghcr.io/github/github-mcp-server`) |

Both authenticate with the **same** env var: `GITHUB_PERSONAL_ACCESS_TOKEN`.

---

## TL;DR — one command

```bash
scripts/setup-github-mcp.sh   # writes token + registers local MCP
# /exit Claude Code, re-launch it
scripts/test-github-mcp.sh    # JSON-RPC test against both servers
```

That's it. The rest of this doc is the manual / explanatory path.

---

## Step 1 — get a token

Cheapest path: re-use your `gh` CLI login.

```bash
gh auth status            # confirm you're logged in
gh auth token             # prints the token to stdout
```

You need scopes covering what you'll do with the MCP. For the standard
read/write tool surface, `repo`, `read:org`, `workflow`, `gist` is plenty.
If `gh auth token` shows a narrower set, run `gh auth refresh -s repo,read:org,workflow,gist`.

If you'd rather use a fine-grained PAT, generate one at
<https://github.com/settings/personal-access-tokens> and substitute it
everywhere `GH_TOKEN=$(gh auth token)` appears below.

---

## Step 2 — add the token to Claude Code

The token lives in the `env` block of `~/.claude/settings.local.json`.
Claude Code reads this file at startup and exports each `env` entry into the
environment of every MCP server process it spawns. That's how:

- the **remote** MCP plugin's `Bearer ${GITHUB_PERSONAL_ACCESS_TOKEN}` header
  gets resolved, and
- the **local** Docker server picks up the token via `docker run -e GITHUB_PERSONAL_ACCESS_TOKEN`
  (Docker reads the env var from its own process — i.e. from Claude Code).

### Manual edit

```bash
TOKEN=$(gh auth token)

jq --arg t "$TOKEN" \
  '.env = ((.env // {}) + {GITHUB_PERSONAL_ACCESS_TOKEN: $t})' \
  ~/.claude/settings.local.json \
  > ~/.claude/settings.local.json.tmp \
  && mv ~/.claude/settings.local.json.tmp ~/.claude/settings.local.json \
  && chmod 600 ~/.claude/settings.local.json
```

After it runs, `~/.claude/settings.local.json` should look like:

```json
{
  "env": {
    "GITHUB_PERSONAL_ACCESS_TOKEN": "gho_…"
  },
  "permissions": { … },
  "prefersReducedMotion": …
}
```

> **Why `settings.local.json` and not `settings.json`?**
> `settings.local.json` is the convention for personal/local-only config in
> Claude Code — it's typically gitignored and is the right home for secrets.
> `settings.json` is fine too if that's already where your `env` block lives.

---

## Step 3 — register the local Docker MCP (optional)

The remote MCP at `api.githubcopilot.com/mcp/` already exists as a Claude
plugin. For a fully **local** server (no external network at runtime once the
image is pulled, full tool surface), add a second MCP entry:

```bash
docker pull ghcr.io/github/github-mcp-server:latest

claude mcp add github-local -- \
  docker run -i --rm \
    -e GITHUB_PERSONAL_ACCESS_TOKEN \
    ghcr.io/github/github-mcp-server:latest
```

The bare `-e GITHUB_PERSONAL_ACCESS_TOKEN` (no `=value`) makes Docker read the
var from its own env — populated from the `env` block above. The token is
**never** written into the MCP config itself.

To remove it later: `claude mcp remove github-local`.

---

## Step 4 — reload

The `env` block is read at Claude Code **startup**. Edits to
`settings.local.json` while a session is running are **not** picked up by
already-spawned MCP processes — you have to restart.

| Action                                  | When you need it                      |
| --------------------------------------- | ------------------------------------- |
| `/exit` then re-launch `claude`         | After changing `env` or adding an MCP |
| `claude mcp list`                       | Verify connection state any time      |

Inside an existing session you cannot "soft-reload" a single MCP server —
quit and re-launch. (The plugin marketplace version of github MCP has its own
reload semantics, but env-var changes still need a restart.)

---

## Step 5 — verify

### Quick: handshake check

```bash
claude mcp list
```

Expect:

```
plugin:github:github: https://api.githubcopilot.com/mcp/ (HTTP) - ✓ Connected
github-local: docker run -i --rm -e GITHUB_PERSONAL_ACCESS_TOKEN ghcr.io/github/github-mcp-server:latest - ✓ Connected
```

### Bulletproof: end-to-end JSON-RPC

```bash
scripts/test-github-mcp.sh
```

This script speaks raw MCP JSON-RPC 2.0 against both servers — independent of
Claude Code — and exits non-zero if either fails. Sequence per server:

1. `initialize` — handshake, capability exchange, session id.
2. `notifications/initialized` — required notification.
3. `tools/call` `get_me` — returns the authenticated user. This is the
   smallest tool that proves both transport and auth work end-to-end.

A successful run looks like:

```
[pass] [remote] initialize OK (server=github-mcp-server, session=abc12345…)
[pass] [remote] get_me → login=peterlodri-sec

[pass] [local] get_me → login=peterlodri-sec

[pass] both servers passed JSON-RPC initialize + get_me
```

---

## Troubleshooting

| Symptom                                                                  | Likely cause / fix                                                                                          |
| ------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| `claude mcp list` shows ✗ for `plugin:github:github`                     | `GITHUB_PERSONAL_ACCESS_TOKEN` not set / Claude Code not restarted. Re-run setup; `/exit` and re-launch.    |
| Remote test: `401 Unauthorized` / `error.code -32001` "auth"             | Token expired or wrong scopes. Run `gh auth refresh -s repo,read:org,workflow,gist`.                        |
| Local test: container produced no stdout                                 | Image pull failed or Docker daemon not running. `docker pull ghcr.io/github/github-mcp-server:latest`.      |
| Token works in `gh` but `setup-github-mcp.sh` says "no gh token"         | `gh auth status` may show "logged in via keyring" but `gh auth token` blocked — run `gh auth login` again.  |
| Token shows up in `git diff` of `settings.local.json`                    | Make sure it's gitignored: `echo 'settings.local.json' >> ~/.claude/.gitignore` (if that dir is a repo).    |
| You're using Apple's `container` runtime, not Docker Desktop             | `alias docker=container` in the shell that runs `setup-github-mcp.sh`, or edit the scripts to call `container`. |

---

## Rotating the token

```bash
gh auth refresh                    # rotate the gh token
scripts/setup-github-mcp.sh        # re-runs jq merge with the new value
# /exit Claude Code, re-launch it
```

Re-running `setup-github-mcp.sh` is safe — it overwrites only the
`GITHUB_PERSONAL_ACCESS_TOKEN` key, leaves everything else in
`settings.local.json` alone.
