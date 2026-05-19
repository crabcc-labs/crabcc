# crabcc-docs

> Private docs companion to [`peterlodri-sec/crabcc`](https://github.com/peterlodri-sec/crabcc). Wired into that repo as a git submodule at `docs/`.

This repo holds research briefs, design notes, and long-form docs that don't ship with the binary but inform what the binary does. The parent repo intentionally excludes `docs/` from `cargo`, `task ci`, repomix bundles, and CI test runs — only the submodule pointer is tracked there. Real content lives here.

## Start here (agents & humans)

| File | Topic |
|------|--------|
| [**`OVERVIEW.md`**](OVERVIEW.md) | **Visual architecture** — colorful Mermaid diagrams (system map, indexing, queries, memory, integrations). Regenerate via Claude Code `/crabcc-generate-overview`. |
| [`../README.md`](../README.md) | Public install, usage, benchmarks (parent repo) |
| [`../crates/crabcc-core/docs/HOW_IT_WORKS.md`](../crates/crabcc-core/docs/HOW_IT_WORKS.md) | Indexing internals (parent repo) |

## Research & design briefs

| File | Topic |
|------|--------|
| [`RESEARCH-tts-voice-control-2026.md`](RESEARCH-tts-voice-control-2026.md) | Cloud-augmented + OSS comparison: near-instant TTS, full-duplex voice control, recommended stacks. |
| [`RESEARCH-tts-voice-control-foss-ios-2026.md`](RESEARCH-tts-voice-control-foss-ios-2026.md) | FOSS-only sibling brief. iPhone↔Mac on-device, no cloud, no API keys, WiFi/Tailscale only. |
| [`desktop/ARCHITECTURE.md`](desktop/ARCHITECTURE.md) | Desktop dashboard notes |
| [`RUST-ANTHOLOGY.md`](RUST-ANTHOLOGY.md) | Rust patterns used in the codebase |

## How this repo is consumed

From the parent crabcc checkout:

```bash
git submodule update --init --recursive   # first time
git submodule update --remote docs        # pull latest from this repo
```

To edit a brief:

```bash
cd docs
git checkout main
# edit, commit, push
cd ..
git add docs && git commit -m "docs: bump to <sha>"   # records new submodule pointer in parent
```

## Visibility

Private. Don't add anything here that needs to ship with the public crabcc binary — except **`OVERVIEW.md`**, which is the shared visual map linked from the parent `README.md` and `AGENTS.md`.
