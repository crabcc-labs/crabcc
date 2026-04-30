# crabcc iTerm2 integration — issue #132

Live crabcc HUD, RPCs, and control sequences wired into iTerm2's Python API.

## Quick start

```bash
crabcc install-iterm2   # copies daemon → AutoLaunch, prints activation steps
```

Then restart iTerm2. The status bar shows `🦀 idle | — | 🟢` immediately.

---

## Prerequisites

| Requirement | Check |
|-------------|-------|
| iTerm2 ≥ 3.5 | `defaults read com.googlecode.iterm2 BuildVersion` |
| Python 3.10+ | `python3 --version` |
| crabcc on PATH | `crabcc --version` |
| crabcc serve running | `curl -s http://localhost:8090/healthz` |

---

## Manual setup (if `crabcc install-iterm2` isn't available yet)

```bash
# 1. Install the iterm2 Python package
pip3 install iterm2

# 2. Copy the daemon
AUTOLAUNCH="$HOME/Library/Application Support/iTerm2/Scripts/AutoLaunch"
mkdir -p "$AUTOLAUNCH"
cp apps/crabcc-iterm2/main.py "$AUTOLAUNCH/crabcc.py"

# 3. Enable Python API in iTerm2
#    Preferences → General → Magic → ✅ Allow full Python API

# 4. Grant Automation permission
#    System Settings → Privacy & Security → Automation → iTerm2 → ✅

# 5. Verify
crabcc doctor iterm2
```

---

## Status-bar HUD

Three slots rendered every 2 seconds:

```
🦀 warp-speed-audit · 4m12s  |  saved 1.2M tok  |  🟢
```

| Slot | Source | Click action |
|------|--------|--------------|
| Active agent + elapsed | `GET /api/agents` | Opens `/live` dashboard |
| Token savings (today) | `crabcc track --json` | — |
| Doctor health glyph | `crabcc doctor --json` | Shows first failure hint |

Glyphs: 🟢 all ok · 🟡 warning · 🔴 failure

---

## RPCs (bind to keys / Touch Bar / status bar click)

Register in **iTerm2 → Preferences → Keys → Key Bindings** → `+` → Action: `Invoke Script Function`:

| Function | Bind suggestion | What it does |
|----------|-----------------|--------------|
| `crabcc_doctor()` | `⌘⇧D` | Runs all doctor checks; alerts on first failure |
| `crabcc_dashboard()` | `⌘⇧L` | Opens `http://localhost:8090/live` in browser |
| `crabcc_search(q="?")` | `⌘⇧F` | Prompts for query → `crabcc memory search` → scratch tab |
| `crabcc_kill_agent(id="?")` | — | Kills agent by ID with confirmation dialog |
| `crabcc_remember()` | `⌘⇧M` | Saves current session content as memory drawer |

---

## Custom control sequences

Emit from any shell alias, editor plugin, or git hook:

```bash
# Look up a symbol — opens result in new tab
printf '\033]1337;Custom=id=crabcc:sym=Store::open\a'

# Trigger re-index after git pull/merge
printf '\033]1337;Custom=id=crabcc:reindex\a'
```

**Shell aliases** (add to `~/.zshrc`):

```zsh
# Look up symbol under cursor or by name
ccs() { printf '\033]1337;Custom=id=crabcc:sym=%s\a' "$1"; }

# Reindex after merge
alias gpm='git pull && printf "\033]1337;Custom=id=crabcc:reindex\a"'
```

**Git post-merge hook** (`~/.git/hooks/post-merge` or repo `.git/hooks/post-merge`):

```bash
#!/usr/bin/env bash
printf '\033]1337;Custom=id=crabcc:reindex\a'
```

---

## Example use-cases

### 1. Audit while you code

Keep the status bar visible. When `task warp-speed-audit` runs in the background,
the HUD shows `🦀 warp-speed-audit · 2m45s` so you know it's running without
switching tabs.

### 2. Instant symbol lookup from vim

Add to `~/.vimrc`:
```vim
" ,cs — look up symbol under cursor in crabcc via iTerm2 control sequence
nnoremap <leader>cs :call system('printf "\033]1337;Custom=id=crabcc:sym=' . expand('<cword>') . '\a"')<CR>
```

### 3. Memory mine on session end

Configure an **iTerm2 Profile Trigger**:
- Regular expression: `\$ logout` (or your prompt pattern)
- Action: `Invoke Script Function`
- Parameters: `crabcc_remember()`

Result: each terminal session body is automatically saved as a crabcc memory drawer.

### 4. Doctor alert on slow CI

Add to `scripts/local-ci.sh` (or any script that might fail):
```bash
# After CI runs — emit control sequence so iTerm2 daemon checks doctor
printf '\033]1337;Custom=id=crabcc:reindex\a'
```

### 5. Quick memory search hotkey

Press `⌘⇧F` → type your query → results appear in a new tab with `jq | less`.
No browser required.

---

## Troubleshooting

```bash
# Check daemon is running
ps aux | grep 'crabcc.py'

# Check Python API is enabled
defaults read com.googlecode.iterm2 EnableAPIServer

# Full doctor check
crabcc doctor iterm2

# Daemon logs
cat ~/Library/Logs/Crabcc/iterm2-daemon.log
```

Common issues:

| Symptom | Fix |
|---------|-----|
| Status bar shows nothing | Restart iTerm2; check Python API pref |
| RPCs do nothing | Grant Automation permission in System Settings |
| `iterm2` import error | `pip3 install iterm2` |
| `crabcc: command not found` inside daemon | Add `~/.cargo/bin` to daemon's PATH |
