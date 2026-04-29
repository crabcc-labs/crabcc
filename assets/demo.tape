# crabcc demo tape — generates assets/demo.gif via vhs
# https://github.com/charmbracelet/vhs
# Render: vhs assets/demo.tape (from repo root)

Output assets/demo.gif

Set Shell zsh
Set FontSize 16
Set Width 1100
Set Height 640
Set Theme "Catppuccin Mocha"
Set Padding 20
Set TypingSpeed 30ms

Hide
Type "export PATH=/Users/peter.lodri/workspace/bin/crabcc/target/release:$PATH"
Enter
Type "cd /Users/peter.lodri/workspace/mc-mothership"
Enter
Type "clear"
Enter
Show

Type "# crabcc — symbol index for AI coding agents"
Sleep 800ms
Enter

# 1. Where is `Assessment` defined?  (10ms vs grep timing out)
Type "crabcc sym Assessment | jq '.[0] | {kind, name, file, line_start}'"
Sleep 600ms
Enter
Sleep 2000ms

# 2. How many call sites of `find_by`?  (14 bytes — 99.98% smaller than full hits)
Type "crabcc callers find_by --count"
Sleep 600ms
Enter
Sleep 1500ms

# 3. Which files reference `Assessment`?  (deduped file list)
Type "crabcc refs Assessment --files-only --limit 5"
Sleep 600ms
Enter
Sleep 2200ms

# 4. List Ruby files under app/models  (constant-time SQL)
Type "crabcc files --under app/models --ext rb --limit 5"
Sleep 600ms
Enter
Sleep 2000ms

# 5. Outline a file  (every symbol, ordered by line)
Type "crabcc outline app/models/assessment.rb | jq '.[0:6]'"
Sleep 600ms
Enter
Sleep 2400ms

Type "# 47–5500x faster than grep -rn  ·  85% fewer bytes  ·  9 MCP tools"
Sleep 1500ms
Enter
Sleep 1500ms
