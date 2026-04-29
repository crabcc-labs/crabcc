---
description: Initialize crabcc symbol index in the current repo and run a first full index.
---

Run these steps:

1. Check that `crabcc` is on PATH. If not, build and link:
   ```
   cd ~/workspace/bin/crabcc && cargo install --path crates/crabcc-cli
   ```
2. From the user's repo root, run `crabcc index`.
3. Report indexed file count, symbol count, and DB size from `.crabcc/index.db`.
4. Suggest the user add `.crabcc/` to `.gitignore` if not already present.
