# crabcc

**Symbol index for AI coding agents.**

4412× faster than grep. 85% fewer bytes sent to the LLM. MCP-native.

## Quick start

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/crabcc-labs/crabcc/main/install.sh | bash

# Index a repo
cd your-project
crabcc index

# Query
crabcc lookup sym ParseError        # find symbol definition
crabcc lookup refs ParseError       # find all references
crabcc lookup outline src/main.rs   # structural outline

# MCP server (for AI agent tools)
crabcc serve
```

## Core commands

| Command | What |
|---------|------|
| `crabcc index` | Build symbol database (~5-30s for monorepo) |
| `crabcc lookup sym <name>` | Find symbol definition |
| `crabcc lookup refs <name>` | Find all references |
| `crabcc lookup outline <file>` | Structural outline |
| `crabcc graph` | Call-graph operations |
| `crabcc memory` | AI memory (past findings) |
| `crabcc serve` | MCP server + localhost viewer |

## Architecture

```
6 crates:
  crabcc-cli     — CLI binary
  crabcc-compact — memory compaction
  crabcc-core    — indexing engine (tree-sitter based)
  crabcc-fetch   — URL fetch + crawl
  crabcc-mcp     — MCP server
  crabcc-memory  — AI memory (BM25 + sqlite-vec)
```

## Install options

```bash
brew install crabcc-labs/tap/crabcc
cargo install crabcc-cli
# or: curl -fsSL https://raw.githubusercontent.com/crabcc-labs/crabcc/main/install.sh | bash
```

## License

MIT
