# crabcc demo tape — generates assets/demo.gif via vhs
# https://github.com/charmbracelet/vhs
# Render: vhs assets/demo.tape (from repo root)

Output assets/demo.gif

Set Shell zsh
Set FontSize 16
Set Width 1100
Set Height 680
Set Theme "Catppuccin Mocha"
Set Padding 24
Set TypingSpeed 28ms

Hide
Type "export PATH=/Users/peter.lodri/workspace/bin/crabcc/target/release:$PATH"
Enter
Type "cd /Users/peter.lodri/workspace/mc-mothership"
Enter
Type "export PROMPT='%F{cyan}❯%f '"
Enter
Type "clear"
Enter
Show

# Title card
Type "printf 'crabcc — symbol index for AI coding agents\n'"
Enter
Sleep 1500ms
Type "clear"
Enter

# 1. Where is Assessment defined?
Type "printf '── Q1: where is the Assessment class defined?\n'"
Enter
Sleep 1100ms
Type "crabcc sym Assessment | jq '.[0] | {kind, name, file, line_start}'"
Enter
Sleep 2200ms
Type "clear"
Enter

# 2. How many call sites of find_by?
Type "printf '── Q2: how many call sites of find_by? (14 bytes vs ~62 KB full hits)\n'"
Enter
Sleep 1300ms
Type "crabcc callers find_by --count"
Enter
Sleep 1700ms
Type "clear"
Enter

# 3. Which files reference Assessment?
Type "printf '── Q3: which files reference Assessment? (deduped, 99.6%% smaller)\n'"
Enter
Sleep 1300ms
Type "crabcc refs Assessment --files-only --limit 5"
Enter
Sleep 2200ms
Type "clear"
Enter

# 4. List Ruby files under app/models
Type "printf '── Q4: list Ruby files under app/models\n'"
Enter
Sleep 1100ms
Type "crabcc files --under app/models --ext rb --limit 5"
Enter
Sleep 2000ms
Type "clear"
Enter

# 5. Outline a file
Type "printf '── Q5: outline a file (every symbol, ordered by line)\n'"
Enter
Sleep 1100ms
Type "crabcc outline app/models/assessment.rb | jq '.[0:6]'"
Enter
Sleep 2400ms
Type "clear"
Enter

# 6. Fuzzy lookup
Type "printf '── Q6: misremembered the name? fuzzy match handles typos\n'"
Enter
Sleep 1100ms
Type "crabcc fuzzy Asseessment --limit 3"
Enter
Sleep 2000ms
Type "clear"
Enter

# 7. Prefix search
Type "printf '── Q7: names starting with… (case-insensitive prefix)\n'"
Enter
Sleep 1100ms
Type "crabcc prefix get_user --limit 5 | jq '.[0:5]'"
Enter
Sleep 2200ms
Type "clear"
Enter

# 8. Tokens-saved tracker
Type "printf '── Q8: how many tokens has crabcc saved this session?\n'"
Enter
Sleep 1100ms
Type "crabcc track --json | jq '.session // .all_time'"
Enter
Sleep 2200ms
Type "clear"
Enter

# Sign-off
Type "printf '\n  47–5500x faster than grep -rn   ·   85%% fewer bytes   ·   9 MCP tools\n\n'"
Enter
Sleep 2500ms
