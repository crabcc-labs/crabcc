#!/usr/bin/env bash
# Detect whether crabcc shell aliases are installed (read-only).
#
# Greps ~/.zshrc / ~/.bashrc / ~/.config/fish/config.fish for the fenced
# `# >>> crabcc-aliases >>>` block written by scripts/install-aliases.sh.
# Prints status per shell rc and exits 0 if at least one shell has the
# block, 1 if none do. Pure detection — never modifies any file.

set -eu

installed=0
any_rc=0
for rc in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.config/fish/config.fish"; do
    if [ -f "$rc" ]; then
        any_rc=1
        if grep -q '# >>> crabcc-aliases >>>' "$rc"; then
            echo "✓ $rc — aliases installed"
            installed=1
        else
            echo "✗ $rc — aliases missing"
        fi
    fi
done

if [ "$any_rc" = "0" ]; then
    echo "no shell rc files found (~/.zshrc, ~/.bashrc, ~/.config/fish/config.fish)"
    exit 1
fi

if [ "$installed" = "1" ]; then
    echo "" && echo "→ at least one shell has aliases. Run \`task aliases\` to (re-)install on others."
    exit 0
else
    echo "" && echo "→ aliases not installed in any shell. Run \`task aliases\` (or \`task install\`) to install."
    exit 1
fi
