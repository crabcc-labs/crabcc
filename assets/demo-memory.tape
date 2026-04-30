# crabcc memory M2 demo tape — generates assets/demo-memory.gif via vhs.
# https://github.com/charmbracelet/vhs
# Render: vhs assets/demo-memory.tape (from repo root)

Output assets/demo-memory.gif

Set Shell zsh
Set FontSize 16
Set Width 1100
Set Height 720
Set Theme "Catppuccin Mocha"
Set Padding 24
Set TypingSpeed 30ms

Hide
Type "export PATH=/Users/peter.lodri/workspace/bin/crabcc/target/release:$PATH"
Enter
Type 'export DEMO=$(mktemp -d)'
Enter
Type 'cd "$DEMO"'
Enter
Type 'git init -q && mkdir -p src docs && echo "fn main() { println!(\"hello\"); }" > src/main.rs && echo "# project notes" > docs/notes.md && echo "// the auth refactor codename is Aurora" > src/auth.rs'
Enter
Type 'mkdir -p sessions && echo ''{"message":{"role":"user","content":"my favourite jogging route is the vondelpark loop"}}'' > sessions/conv.jsonl && echo ''{"message":{"role":"assistant","content":"vondelpark loop, noted"}}'' >> sessions/conv.jsonl'
Enter
Type "export PROMPT='%F{cyan}❯%f '"
Enter
Type "clear"
Enter
Show

# Title card
Type "printf 'crabcc memory — local-first, per-repo AI memory layer\n'"
Enter
Sleep 1500ms
Type "clear"
Enter

# 1. Init
Type "printf '── 1. open the per-repo memory store\n'"
Enter
Sleep 900ms
Type "crabcc memory init"
Enter
Sleep 1500ms
Type "clear"
Enter

# 2. Mine project
Type "printf '── 2. mine the repo (one drawer per text file, idempotent)\n'"
Enter
Sleep 1100ms
Type "crabcc memory mine project"
Enter
Sleep 1700ms
Type "clear"
Enter

# 3. Mine sessions
Type "printf '── 3. mine Claude Code transcripts (one drawer per turn pair)\n'"
Enter
Sleep 1100ms
Type "crabcc memory mine sessions sessions/"
Enter
Sleep 1700ms
Type "clear"
Enter

# 4. Search the repo wing
Type "printf '── 4. hybrid search (BM25 + cosine via RRF) — find the auth codename\n'"
Enter
Sleep 1300ms
Type "crabcc memory search Aurora --limit 2 | jq '.hits[] | {source_id}'"
Enter
Sleep 2400ms
Type "clear"
Enter

# 5. Search the sessions wing
Type "printf '── 5. recall something I said last week to Claude\n'"
Enter
Sleep 1300ms
Type "crabcc memory search 'jogging route' --wing session --limit 2 | jq '.hits[] | {source_id, body}'"
Enter
Sleep 2400ms
Type "clear"
Enter

# 6. Re-run mine = idempotent
Type "printf '── 6. re-run the miner — every drawer dedups (zero new rows)\n'"
Enter
Sleep 1300ms
Type "crabcc memory mine project"
Enter
Sleep 1700ms
Type "clear"
Enter

# 7. Bench gate
Type "printf '── 7. the LongMemEval R@5 ≥ 96.6%% gate (closes issue #2)\n'"
Enter
Sleep 1300ms
Type "cd /Users/peter.lodri/workspace/bin/crabcc && cargo run --release -q -p crabcc-memory-bench -- --output /tmp/memory.ndjson 2>&1 | tail -2"
Enter
Sleep 3500ms

# Sign-off
Type "clear"
Enter
Type "printf '\n  10 CLI cmds  ·  10 MCP tools  ·  hybrid BM25⊕vector  ·  R@5 ≥ 96.6%%\n\n'"
Enter
Sleep 2500ms
