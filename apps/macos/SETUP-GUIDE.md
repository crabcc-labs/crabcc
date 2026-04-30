# macOS app setup guide — verifying PR #193

Step-by-step for verifying `task macos-app` end-to-end on your Mac.
Each step lists what to expect and what to send back to me if it
breaks. Time budget: ~10–15 min the first time (mostly waiting for
Tuist to resolve SPM packages).

> If anything in this guide is unclear or a step fails, copy the
> failing command + the last ~30 lines of its output and paste it
> back. Don't try to "fix forward" — the failure mode tells me which
> assumption I got wrong.

## 0. Prerequisites — verify first, install if missing

Run these four checks. All four should print a version, not "command
not found":

```bash
sw_vers -productVersion           # macOS 13.0+ required
xcodebuild -version                # Xcode 15.4+ required (16+ ideal)
swift --version                    # Swift 5.10+ required
which jq && jq --version           # for the JSON validation step
```

| Tool | Why | Fix if missing |
|---|---|---|
| macOS 13.0+ | `MenuBarExtra` (SwiftUI), our deployment target | Update macOS via System Settings |
| Xcode 15.4+ | Swift 5.10, TCA macros, Tuist 4.x | App Store → install Xcode |
| Swift CLI | Resolves SPM deps via `tuist install` | Comes with Xcode |
| `jq` | Validates `.tools` JSON | `brew install jq` |

> **First-time-only**: after installing Xcode, run
> `sudo xcode-select -s /Applications/Xcode.app` and
> `sudo xcodebuild -license accept` once.

## 1. Install Tuist

Two paths — **pick one**, not both:

### Option A — `mise` (preferred; matches `.tools` convention)

```bash
brew install mise                             # if not already installed
mise install tuist@4                          # pulls latest 4.x
mise use --global tuist@4                     # makes it the default
tuist version                                 # should print 4.x.x
```

### Option B — `brew` (faster if you don't want mise)

```bash
brew install tuist
tuist version
```

### Option C — manual installer (if A and B both fail)

```bash
curl -Ls https://install.tuist.io | bash
```

## 2. Pull the branch

```bash
cd ~/workspace/bin/crabcc                     # or wherever you have it
git fetch origin
git checkout feat/192-native-apple-stack-phase-0
git pull
```

You should now see `apps/macos/` populated with `Project.swift`,
`Sources/`, `Tests/`, `Tuist/`, etc.

## 3. Smoke-test the Taskfile entries (no build yet)

```bash
task --list-all | grep macos-app
```

Expected — five new targets:

```
* macos-app:           One-shot — install deps, generate, build, test (#192)
* macos-app-build:     Build apps/macos/ via xcodebuild (#192) — Release configuration
* macos-app-generate:  Generate apps/macos/Crabcc.xcodeproj from Project.swift (#192)
* macos-app-install:   Resolve SPM dependencies for apps/macos via Tuist (#192)
* macos-app-test:      Run TCA TestStore tests for apps/macos (#192)
```

If those don't show up: `task` itself might not be installed
(`brew install go-task`), or you're on the wrong branch.

## 4. Resolve SPM dependencies

```bash
task macos-app-install
```

What this does: runs `tuist install` inside `apps/macos/`. Tuist
fetches the four declared packages into
`apps/macos/Tuist/.swiftpm/`:

- swift-composable-architecture (TCA) ≥ 1.16.0
- lottie-ios ≥ 4.5.0
- swift-async-queue ≥ 0.5.0

**Expected:** roughly 30–90 seconds the first time; second run is
cached and near-instant. Should end with no error.

**If it fails:**
- `Could not resolve package` → likely a network issue or a typo in
  `apps/macos/Tuist/Package.swift`. Send me the full error.
- `tuist: command not found` → step 1 didn't take; re-run.

## 5. Generate the Xcode project

```bash
task macos-app-generate
```

What this does: `tuist generate --no-open` inside `apps/macos/`.
Produces `apps/macos/Crabcc.xcodeproj` (gitignored).

**Expected:** a few seconds. Should end with something like
`Project generated.`

**Verify:**
```bash
ls apps/macos/Crabcc.xcodeproj                # directory should exist
```

## 6. Build the app

```bash
task macos-app-build
```

What this does: `xcodebuild ... -configuration Release build`.

**Expected:** 1–5 minutes the first time (Xcode compiles TCA + Lottie
+ AsyncQueue from source the first build, caches them after). You'll
see a lot of output ending with `BUILD SUCCEEDED`.

The produced `.app`:

```bash
ls apps/macos/Derived/Build/Products/Release/Crabcc.app
```

**If it fails:**
- `error: Macro "X" must be enabled before it can be used` → Xcode 15+
  gates SPM macros behind a manual "Trust & Enable" prompt that
  headless builds can't surface. The `task macos-app-build` invocation
  passes `-skipMacroValidation` to handle this; if you still see this
  error, your Xcode is older than 15.4 — upgrade.
- `error: 'main' attribute cannot be used in a module that contains
  top-level code` → SourceKit was right and I was wrong; tell me, I
  fix.
- `error: no such module 'X'` → SPM resolution issue; re-run step 4.
- `Swift Compiler Error: Cannot find type 'Reducer' in scope` → TCA
  version mismatch; tell me the version Tuist resolved
  (`cat apps/macos/Tuist/.swiftpm/Package.resolved | jq '.pins[] | select(.identity=="swift-composable-architecture")'`).

## 7. Run the TCA TestStore tests

```bash
task macos-app-test
```

What this does: `tuist test`. Runs `StickyFeatureTests.testBodyChange`.

**Expected:** one passing test. Output looks roughly like:

```
Test Suite 'StickyFeatureTests' passed.
     Executed 1 test, with 0 failures (0 unexpected) in 0.001 (0.002) seconds
```

## 8. (Optional) launch the placeholder MenuBarExtra app

```bash
open apps/macos/Derived/Build/Products/Release/Crabcc.app
```

You should see a new menubar icon (📦 shipping box). Click it — the
menu shows:

```
Crabcc — Tuist scaffold
Phase 0 of issue #192 — TCA + Tuist + Lottie + AsyncQueue
─────
Quit                                                                ⌘Q
```

This is the **scaffold**, not the production menubar. The production
menubar (the one with Active repo / Scheduled Tasks / Telegram Bot
section / Sticky / etc.) is still served by the legacy
`installer/Crabcc.app` and `task dmg` — Phase 1+ migrates that over.

Quit it: `⌘Q` from the menu.

## 9. Confirm legacy build still works

```bash
task dmg                                      # the production .dmg path
ls dist/crabcc-*.dmg                          # should be there
```

This proves Phase 0 didn't regress the working pipeline.

## 10. Approve the draft PR

Once steps 4–7 succeed and step 9 still works:

```bash
gh pr ready 193                               # un-draft
gh pr review 193 --approve --body "Phase 0 verified end-to-end on macOS X / Xcode Y / Tuist Z"
```

## What to send me back

Whichever applies:

- **Everything passed** → "all green, ready to merge". I'll squash + merge.
- **Step N failed** → send:
  1. The exact command (e.g. `task macos-app-install`)
  2. The last ~30 lines of output
  3. The output of `tuist version`, `xcodebuild -version`, `swift --version`
- **Something unexpected happened** (e.g. a different menubar icon
  showed up, a TestStore test you didn't expect ran) → describe it; I
  audit and adjust the PR.

## Cleanup

Nothing to clean up — Tuist's outputs are all gitignored. To start
from scratch any time:

```bash
rm -rf apps/macos/Crabcc.xcodeproj apps/macos/Derived apps/macos/Tuist/.swiftpm
task macos-app                                # rebuilds everything
```
