# LangChain + LangGraph + LangSmith

Python helpers that expose crabcc symbol lookups as LangChain tools and a
minimal LangGraph loop.

## Quick start

```bash
cd ~/.crabcc/integrations/langchain   # after install-integrations --target langchain
pip install -e .

export CRABCC_ROOT=/path/to/your/repo
export LANGCHAIN_TRACING_V2=true
export LANGSMITH_API_KEY=lsv2_...
export LANGSMITH_PROJECT=crabcc-dev

python -c "
from langchain_openai import ChatOpenAI
from crabcc_langchain import build_lookup_graph, crabcc_sym
from crabcc_langchain.graph import demo_prompt

model = ChatOpenAI(model='gpt-4o-mini')
graph = build_lookup_graph(model)
out = graph.invoke({'messages': [demo_prompt()]})
print(out['messages'][-1].content)
"
```

## LangSmith experiments (batch eval)

The repo ships bash helpers under `tools/orchestrator/`:

```bash
export LANGSMITH_API_KEY=...
wave="$(tools/orchestrator/import-dataset.sh my-dataset swe-fast)"
# ... run workers ...
tools/orchestrator/upload-experiment.sh "$wave"
```

See [`tools/orchestrator/README.md`](../../../tools/orchestrator/README.md).

## Tools

| Tool | CLI equivalent |
|------|----------------|
| `crabcc_sym` | `crabcc sym NAME` |
| `crabcc_refs` | `crabcc refs NAME --limit N` |
| `crabcc_callers` | `crabcc callers NAME` |
| `crabcc_outline` | `crabcc outline PATH` |

Set `CRABCC_BIN` / `CRABCC_ROOT` to override binary and repo root.
