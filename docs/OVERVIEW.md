# crabcc — visual overview

> Diagram-first map of the system. For prose traces and per-command mechanics see
> [README § Architecture](../README.md#architecture). For engine-room detail see
> [`crates/crabcc-core/docs/HOW_IT_WORKS.md`](../crates/crabcc-core/docs/HOW_IT_WORKS.md).

**Regenerate or extend:** run `/crabcc-generate-overview` in Claude Code (see
[`commands/crabcc/generate/overview.md`](../commands/crabcc/generate/overview.md)).

---

## 1. What crabcc is (one picture)

```mermaid
%%{init: {'theme': 'base', 'themeVariables': {
  'primaryColor': '#e85d04',
  'primaryTextColor': '#1a1a1a',
  'primaryBorderColor': '#dc2f02',
  'secondaryColor': '#4cc9f0',
  'secondaryTextColor': '#0d1b2a',
  'tertiaryColor': '#b5179e',
  'lineColor': '#495057',
  'fontFamily': 'ui-sans-serif, system-ui, sans-serif'
}}}%%
flowchart TB
  subgraph agents["🤖 Coding agents"]
    CC[Claude Code]
    CU[Cursor]
    MCP[MCP clients]
  end

  subgraph crabcc["🦀 crabcc surface"]
    CLI[crabcc CLI]
    SRV[crabcc-mcp stdio]
    SKILL[skill + slash commands]
  end

  subgraph libs["📚 Rust libraries"]
    CORE[crabcc-core<br/>index · query · graph · FTS]
    MEM[crabcc-memory<br/>Palace · hybrid search]
  end

  subgraph disk["💾 On disk"]
    IDX[".crabcc/index.db<br/>tantivy/ · graph.json"]
    HOME["$CRABCC_HOME/repos/&lt;slug&gt;/memory.db"]
  end

  CC --> CLI
  CC --> SRV
  CU --> MCP
  MCP --> SRV
  SKILL --> CLI

  CLI --> CORE
  CLI --> MEM
  SRV --> CORE
  SRV --> MEM

  CORE --> IDX
  MEM --> HOME

  classDef agent fill:#7209b7,stroke:#560bad,color:#fff
  classDef surface fill:#f77f00,stroke:#d62828,color:#1a1a1a
  classDef lib fill:#06d6a0,stroke:#118ab2,color:#073b4c
  classDef store fill:#ffd166,stroke:#ef476f,color:#1a1a1a

  class CC,CU,MCP agent
  class CLI,SRV,SKILL surface
  class CORE,MEM lib
  class IDX,HOME store
```

| Layer | Role |
|-------|------|
| **Agents** | Claude Code, Cursor, LangChain, etc. — never walk the repo with `grep -rn` for symbols |
| **Surface** | Thin dispatch: argv (CLI) or JSON-RPC 2.0 (MCP); same code paths |
| **Libraries** | `crabcc-core` = symbol index; `crabcc-memory` = per-repo drawers + hybrid retrieval |
| **Disk** | Repo-local index + home-dir memory (worktrees share memory by remote URL hash) |

---

## 2. Workspace map (crates & apps)

```mermaid
%%{init: {'theme': 'dark'}}%%
flowchart LR
  subgraph workspace["Cargo workspace"]
    CLI[crabcc-cli]
    CORE[crabcc-core]
    MCP[crabcc-mcp]
    MEM[crabcc-memory]
    FETCH[crabcc-fetch]
    GF[crabcc-godfather]
    LSP[ucracc-lsp]
    CHR[crabcc-chrome]
  end

  subgraph standalone["Standalone builds"]
    VIZ[crabcc-viz<br/>call-graph UI]
    DESK[crabcc-desktop<br/>GPUI dashboard]
    TG[crabcc-telegram]
  end

  subgraph apps["Apps / sidecars"]
    HITL[crabcc-hitl-agent]
    NOTIFY[notify-ext-poc]
  end

  CLI --> CORE
  CLI --> MEM
  CLI --> MCP
  MCP --> CORE
  MCP --> MEM
  LSP --> CORE
  CHR --> CORE
  VIZ -.->|HTTP/SSE| CORE
  DESK -.->|loopback| VIZ

  classDef core fill:#2d6a4f,stroke:#95d5b2,color:#fff
  classDef edge fill:#457b9d,stroke:#a8dadc,color:#fff
  classDef solo fill:#6c757d,stroke:#adb5bd,color:#fff

  class CORE,MEM,MCP,CLI core
  class LSP,CHR,FETCH,GF edge
  class VIZ,DESK,TG,HITL,NOTIFY solo
```

Excluded from the workspace (build in-crate): `crabcc-viz`, `crabcc-desktop`, `apps/crabcc-telegram` — see comments in root `Cargo.toml`.

---

## 3. Data on disk

```mermaid
%%{init: {'theme': 'forest'}}%%
flowchart TB
  subgraph repo["&lt;your-repo&gt;/"]
    CC[".crabcc/"]
    CC --> DB[(index.db<br/>files · symbols · edges)]
    CC --> TAN[tantivy/<br/>fuzzy + prefix]
    CC --> GJ[graph.json<br/>call-graph sidecar]
    CC --> FSST[fsst.symbols<br/>signature codec]
    CC --> TRK[track.json<br/>token savings]
  end

  subgraph home["$CRABCC_HOME (~/.crabcc)"]
    MEMDB[(repos/&lt;slug&gt;-&lt;hash6&gt;/memory.db<br/>drawers + FTS5 + vectors)]
    INT[integrations/<br/>langchain · hooks]
    LOG[usage.log]
  end

  classDef sql fill:#4361ee,stroke:#3a0ca3,color:#fff
  classDef file fill:#4ea8de,stroke:#023e8a,color:#023e8a

  class DB,MEMDB sql
  class TAN,GJ,FSST,TRK,INT,LOG file
```

| Path | Built by | Queried by |
|------|----------|------------|
| `index.db` | `crabcc index` / `refresh` | `sym`, `refs`, `callers`, `outline`, `files` |
| `tantivy/` | index + `fts-rebuild` | `fuzzy`, `prefix` |
| `graph.json` | `crabcc graph build` | `graph walk`, viz, LSP call hierarchy |
| `memory.db` | `memory remember` / `mine` | `memory search` (hybrid RRF) |

---

## 4. Indexing pipeline

```mermaid
%%{init: {'theme': 'neutral'}}%%
flowchart LR
  W[walker<br/>gitignore-aware] --> P[tree-sitter<br/>per language]
  P --> E[extract<br/>symbols + edges]
  E --> S[(SQLite upsert)]
  E --> ED[edges table<br/>O files not O n²]
  S --> FTS[Tantivy rebuild<br/>on full index]
  ED --> GB[graph build<br/>optional]

  style W fill:#ffe066,stroke:#ff6b6b
  style P fill:#ff9f1c,stroke:#e85d04
  style E fill:#06d6a0,stroke:#1b4332
  style S fill:#4cc9f0,stroke:#0077b6
  style FTS fill:#c77dff,stroke:#7b2cbf
  style GB fill:#80ffdb,stroke:#40916c
```

Languages today: TypeScript, TSX, JavaScript, Ruby, Rust, Go, Python (+ more via grammar crates in `Cargo.toml`).

---

## 5. Query router (which path runs?)

```mermaid
%%{init: {'theme': 'base'}}%%
flowchart TD
  Q[Agent question] --> D{Intent?}

  D -->|definition| SYM[sym → SQL name = ?]
  D -->|references| REF{Shape?}
  D -->|callers| CAL{edges populated?}
  D -->|typo name| FUZ[fuzzy → Tantivy]
  D -->|prefix| PRE[prefix → Tantivy]
  D -->|file skeleton| OUT[outline → SQL by file_id]
  D -->|past notes| MEM[memory search → RRF]

  REF -->|full hits| R1[refs: memchr + tree-sitter]
  REF -->|file list only| R2[refs --files-only early stop]
  CAL -->|yes| C1[SQL on edges.dst_name]
  CAL -->|no| C2[ast-grep patterns over files]
  CAL -->|--count| C3[COUNT * only → tiny JSON]

  SYM --> OUTJSON[sonic-rs JSON stdout]
  R1 --> OUTJSON
  R2 --> OUTJSON
  C1 --> OUTJSON
  C2 --> OUTJSON
  C3 --> OUTJSON
  FUZ --> OUTJSON
  PRE --> OUTJSON
  OUT --> OUTJSON
  MEM --> OUTJSON

  classDef fast fill:#06d6a0,stroke:#1b4332,color:#073b4c
  classDef slow fill:#ffd166,stroke:#e85d04,color:#1a1a1a
  classDef mem fill:#b5179e,stroke:#7209b7,color:#fff

  class SYM,C1,C3,FUZ,PRE,OUT fast
  class R1,R2,C2 slow
  class MEM mem
```

**Rule of thumb:** prefer `callers --count` and `refs --files-only --limit N` before full scans — token-shaped output is the product feature.

---

## 6. Memory: hybrid search (RRF)

```mermaid
%%{init: {'theme': 'dark'}}%%
flowchart TB
  Q[query string] --> EMB[Embedder<br/>HashEmbedder or FastEmbed]
  Q --> LEX[FTS5 BM25<br/>drawers_fts]

  EMB --> VEC[vector KNN<br/>cosine / sqlite-vec]
  LEX --> Lhits[lexical hits]
  VEC --> Vhits[vector hits]

  Lhits --> RRF[Reciprocal Rank Fusion<br/>k = 60]
  Vhits --> RRF
  RRF --> TOP[top-K drawers JSON]

  classDef q fill:#f72585,stroke:#b5179e,color:#fff
  classDef rank fill:#4cc9f0,stroke:#0077b6,color:#0d1b2a
  classDef fuse fill:#ffd60a,stroke:#ffc300,color:#1a1a1a

  class Q q
  class EMB,LEX,VEC rank
  class RRF,TOP fuse
```

Modes: `hybrid` (default with embeddings), `lexical`, `vector` — see `crabcc memory search --mode`.

---

## 7. Agent integrations

```mermaid
%%{init: {'theme': 'base'}}%%
mindmap
  root((crabcc integrations))
    Claude Code
      install-claude
      skill/crabcc
      slash commands
      hooks optional
    Cursor
      .cursor/mcp.json
      project MCP
    LangChain
      ~/.crabcc/integrations/langchain
      memory tools
    OS daemon
      launchd / systemd
    Local LLM
      Ollama stack
      LiteLLM :4000
```

Install everything:

```bash
crabcc setup install-integrations --target all --project --yes
```

Guide: [`install/integrations.md`](../install/integrations.md).

---

## 8. Ollama agent stack

```mermaid
%%{init: {'theme': 'dark'}}%%
sequenceDiagram
  participant Agent as Claude Code / crabcc agent
  participant Proxy as free-claude-code
  participant LLM as LiteLLM :4000
  participant Gate as Caddy :11435
  participant Oll as Ollama

  Agent->>Proxy: Anthropic-compat API
  Proxy->>LLM: route + prompt cache
  LLM->>Gate: Bearer auth
  Gate->>Oll: qwen3.5:35b-a3b-coding-nvfp4
  Oll-->>Agent: SSE tokens
```

Bootstrap: `task setup` · Run: `crabcc agent --run "…" --backend ollama`

---

## 9. CLI vs MCP (same engine)

```mermaid
%%{init: {'theme': 'neutral'}}%%
flowchart LR
  subgraph in["Input"]
    A1[argv clap]
    A2[JSON-RPC tools]
  end

  subgraph dispatch["Dispatch"]
    D[Cmd / tool handler]
  end

  subgraph out["Output"]
    J[sonic-rs JSON]
    O[stdout / MCP content]
  end

  A1 --> D
  A2 --> D
  D --> J --> O

  style A1 fill:#e9c46a
  style A2 fill:#e9c46a
  style D fill:#2a9d8f,color:#fff
  style J fill:#264653,color:#fff
```

MCP tools mirror CLI subcommands (`sym`, `refs`, `callers`, `memory.*`, …). Optional `cwd` on memory tools walks up to `.git` for the correct palace.

---

## 10. Performance snapshot (why agents care)

```mermaid
%%{init: {'theme': 'base'}}%%
xychart-beta
    title "Relative latency vs grep (mc-mothership ~13k files)"
    x-axis ["sym", "callers --count", "refs --files-only", "grep -rn"]
    y-axis "faster →" 0 --> 5000
    bar [4000, 5500, 200, 1]
```

| Output shape | Typical size vs grep |
|--------------|----------------------|
| `sym Foo` | typed JSON, µs–ms |
| `callers X --count` | ~14 bytes |
| `refs X --files-only --limit 5` | hundreds of bytes vs MB of text |

Full tables: [README § Bench results](../README.md#bench-results-mc-mothership-13k-indexed-files).

---

## 11. Session bootstrap (`crabcc go`)

```mermaid
%%{init: {'theme': 'forest'}}%%
flowchart LR
  GO[crabcc go] --> I[index / refresh]
  I --> G[graph build]
  G --> M[memory warm]
  M --> CL[launch Claude Code<br/>--effort max]

  style GO fill:#f72585,color:#fff
  style CL fill:#4361ee,color:#fff
```

One command to index, build the call graph, and open Claude with the skill loaded.

---

## 12. Doc map (where to read next)

```mermaid
flowchart TD
  OV[docs/OVERVIEW.md<br/>you are here]
  README[README.md<br/>install · usage · traces]
  HOW[crabcc-core/HOW_IT_WORKS.md<br/>schema · extract · tests]
  AGENTS[AGENTS.md<br/>agent conventions]
  INT[install/integrations.md]
  EX[examples/CLI.md · MCP.md · memory.md]

  OV --> README
  OV --> HOW
  OV --> AGENTS
  README --> EX
  AGENTS --> INT
```

---

## Legend

| Symbol | Meaning |
|--------|---------|
| 🦀 | Rust crate or binary |
| 💾 | Persistent store |
| 🤖 | External coding agent |
| Solid arrow | compile-time or direct call |
| Dotted arrow | HTTP/SSE / optional integration |

*Last updated with repo architecture at v4.x. Diagrams render on GitHub, in Cursor, and in Claude Code markdown previews.*
