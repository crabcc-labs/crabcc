# `apps/macos/` — Tuist scaffold for `Crabcc.app`

Phase 0 of [#192](https://github.com/peterlodri-sec/crabcc/issues/192) —
introduces the **TCA + Tuist + Lottie + AsyncQueue** stack as a parallel
build path. The legacy `swiftc -O -parse-as-library *.swift` line in
[`scripts/build-dmg.sh`](../../scripts/build-dmg.sh) **is unchanged**
through this phase — both pipelines coexist so Phase 1+ can migrate
incrementally without breaking the working build.

## Layout

```
apps/macos/
├── Project.swift                 # Tuist 4.x project declaration
├── Tuist/
│   └── Package.swift             # Master SPM manifest (TCA, Lottie, AsyncQueue)
├── Sources/
│   ├── CrabccApp.swift           # @main SwiftUI MenuBarExtra entry point
│   └── Features/
│       └── StickyFeature.swift   # TCA Reducer scaffold + linker probes
├── Tests/
│   └── StickyFeatureTests.swift  # TCA TestStore stub
├── Resources/                    # (.lottie animations land here in Phase 4)
└── .gitignore                    # Tuist + Xcode outputs
```

## Prerequisites

- macOS 13+ (matches the deployment target)
- Xcode 15.4+ (Swift 5.10+) — `xcode-select --install` for CLT, or full Xcode
- Tuist CLI — install via:
  ```bash
  mise install tuist          # preferred (mise is in .tools)
  brew install tuist           # alternative
  ```

## Build

```bash
cd apps/macos
tuist install                # resolves SPM deps into Tuist/.swiftpm/
tuist generate               # produces Crabcc.xcodeproj (gitignored)
xcodebuild \
    -project Crabcc.xcodeproj \
    -scheme Crabcc \
    -configuration Release \
    build
```

The built binary lives at
`apps/macos/Derived/Build/Products/Release/Crabcc.app`. Phase 1 of #192
wires `scripts/build-dmg.sh` to produce the staged `.app` from this
output instead of the current `swiftc` line.

## Test

```bash
tuist test                   # runs CrabccTests target
```

## Why both pipelines exist right now

The legacy single-file `swiftc *.swift` build in `build-dmg.sh` produces
the production `.dmg` shipped today. Switching it over in one PR is too
much to verify — the migration plan in #192 spans five phases. Phase 0
(this PR) only proves the new stack scaffolds correctly. The legacy
files at `installer/Crabcc.app/Contents/MacOS/*.swift` are **not** moved
or modified.

When Phase 1 lands, this README's "Build" section becomes the only path,
the legacy `swiftc` line in `build-dmg.sh` is replaced, and the
`installer/Crabcc.app/Contents/MacOS/*.swift` sources move under
`apps/macos/Sources/`.

## See also

- [`skill/crabcc-app-stack/SKILL.md`](../../skill/crabcc-app-stack/SKILL.md)
  — full stack rationale + 20-entry cheatsheet for agents working in
  this directory
- [#192](https://github.com/peterlodri-sec/crabcc/issues/192) — the
  multi-phase RFC
- AGENTS.md → workspace layout
- CLAUDE.md → eat-your-own-dogfood + `.tools` cross-reference
