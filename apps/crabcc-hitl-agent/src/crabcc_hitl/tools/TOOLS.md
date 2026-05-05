# HITL agent tools — brainstorm + ship order

> Phase 0 implements only `fetch_url`. The rest are sketches with
> file paths reserved + signatures so Phase 1 can land them
> incrementally without re-thinking shape.

## Phase 1 — crabcc surface (highest leverage)

The bot is embedded in a code-search workspace. Most user prompts will be variants of "where is X / what calls Y / show me Z". These wrap the existing `crabcc --mcp-http` server (started as a sibling container in `apps/crabcc-agents/docker-compose.yml` once `--mcp-http` ships).

| File | Tool | Wraps | Notes |
|---|---|---|---|
| `crabcc_sym.py` | `crabcc_sym(name)` | `crabcc sym <name>` | Most common entry point. |
| `crabcc_refs.py` | `crabcc_refs(name, files_only=False)` | `crabcc refs <name>` | `files_only` saves tokens. |
| `crabcc_callers.py` | `crabcc_callers(name)` | `crabcc callers <name>` | Pure-SQL edge query. |
| `crabcc_files.py` | `crabcc_files(under, ext=None, limit=50)` | `crabcc files --under` | Replaces `find` / `ls -R`. |
| `crabcc_outline.py` | `crabcc_outline(file)` | `crabcc outline <file>` | Read before whole-file. |
| `crabcc_fuzzy.py` | `crabcc_fuzzy(pattern)` | `crabcc fuzzy` | Levenshtein-2 fallback. |

These all hit `${CRABCC_HITL_MCP_BASE_URL}` (settings field already exists). Auth via the matching bearer token. One thin Python helper handles the MCP-HTTP envelope so each tool module stays one screen of code.

## Phase 1 — memory layer

| File | Tool | Wraps |
|---|---|---|
| `memory_remember.py` | `memory_remember(key, body)` | `crabcc memory remember` |
| `memory_search.py` | `memory_search(query, mode="hybrid", limit=5)` | `crabcc memory search` |
| `memory_list.py` | `memory_list(limit=20)` | `crabcc memory list` |

Lets the agent persist context across sessions — _"remember that
@peter prefers wooorm/markdown-rs over comrak"_ → next session
recalls.

## Phase 1 — utility

| File | Tool | Why useful |
|---|---|---|
| `now.py` | `now()` | ISO-8601 UTC timestamp — model usually doesn't know its current time. |
| `git_status.py` | `git_status(repo)` | Read-only `git status -sb` for the cwd repo. Useful for "what's uncommitted right now?" |
| `webhook_post.py` | `webhook_post(url, payload)` | Notify external webhooks with HITL approval. |

## Phase 2 — risky / approval-gated

| File | Tool | Why HITL gates it |
|---|---|---|
| `shell_exec.py` | `shell_exec(cmd)` | Runs a shell command. **Always** triggers the approval flow before execution. |
| `fs_read.py` / `fs_write.py` | Sandboxed read/write under `${CRABCC_HITL_SANDBOX_DIR}`. | Approval required for writes; reads gated on path prefix. |
| `code_run.py` | Run a code snippet inside a microsandbox container. | Trusted runtime via the existing `microsandbox` skill. |

## Phase 2 — search + research

| File | Tool | Backend |
|---|---|---|
| `web_search.py` | `web_search(query, n=5)` | DuckDuckGo or Brave search API. |
| `github_lookup.py` | `github_issue(repo, issue_n)` / `github_pr(repo, pr_n)` | `gh` CLI shellout (already on the runtime image). |

## Implemented (Phase 0)

- **`fetch_url.py`** — download → SSRF-guarded → markitdown → markdown. Handles HTML, PDF, DOCX, XLSX, PPTX. SSRF policy mirrors the Rust `crabcc-fetch::is_ingest_safe_url`.

## Tool-shape conventions

- One async function per tool, named after the tool.
- Args are primitive types (`str`, `int`, paths) — the OpenAI Agents SDK derives the JSON schema directly from the signature.
- Docstring is LLM-facing: first line is the picker description, follow-up paragraphs describe args + return shape.
- Return a structured Pydantic model (`FetchResult`-style). Errors come back as `ok=False` with a `.error` field — agents handle returned errors gracefully; raised exceptions abort the agent loop with much less context.
- No I/O side effects on disk except in the explicit `fs_*` family (which are HITL-gated).

## Wiring (Phase 1)

```python
# apps/crabcc-hitl-agent/src/crabcc_hitl/llm.py — Phase 1 sketch
from agents import Agent, function_tool
from .tools import fetch_url, crabcc_sym, crabcc_refs, ...  # noqa

self._agent = Agent(
    name="crabcc-helper",
    instructions=settings.system_prompt,
    model=self._model,
    tools=[
        function_tool(fetch_url),
        function_tool(crabcc_sym),
        function_tool(crabcc_refs),
        # ...
    ],
)
```

Approval-gated tools (Phase 2) wrap their `function_tool` call in a HITL middleware that emits a `PendingApproval` event to the bot before invoking the underlying impl.
