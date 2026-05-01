# crabcc-notify-ext-poc — placeholder

Future home of the **`UNNotificationContentExtension`** (Swift +
Storyboard / SwiftUI) that renders custom-styled crabcc notification
banners on macOS / iOS / iPadOS.

> Phase-0 placeholder only — there is no Xcode project here yet. The
> entire architecture, rationale, and constraints are documented in
> [`../../crates/crabcc-desktop/docs/RESEARCH-apple-rich-notifications-dossier.md`](../../crates/crabcc-desktop/docs/RESEARCH-apple-rich-notifications-dossier.md)
> and the [phasing in the research doc](../../crates/crabcc-desktop/docs/RESEARCH-native-desktop-and-rich-notifications.md#track-c--native-macos-rich-notifications).

## Why this lives outside `crates/`

`UNNotificationContentExtension` is a strictly native target produced
by Xcode. It compiles to a `.appex` bundle that must be embedded
inside the host `.app` at `Contents/PlugIns/<name>.appex`. Cargo can't
build that target directly; we'll drive the Swift compile via a
`setup-apple-extension.sh` script invoked from a `build.rs` or a
top-level Taskfile target.

## Future layout

```
apps/crabcc-notify-ext-poc/
  README.md                 # this file
  Info.plist                # UNNotificationExtensionCategory + …DefaultContentHidden
  CrabccNotifyExt.entitlements   # com.apple.security.application-groups: group.dev.crabcc
  Sources/
    NotificationViewController.swift    # NSViewController / UIViewController
    NotificationView.swift              # SwiftUI view, reads App Group state
  Resources/
    MainInterface.storyboard            # if we go Storyboard route
```

## Triggering

The Rust host (in `crates/crabcc-desktop` once phase A lands, or in
`crates/crabcc-cli` for shell-fired notifications) registers a
`UNNotificationCategory` with identifier `crabcc.notify.rich` (or
similar) and submits a request whose `content.categoryIdentifier`
matches. The OS routes the banner to this extension iff the bundle
is properly signed + nested in `PlugIns/`.

## Build pipeline

The build is **two-phase**:

1. `cargo build --release -p crabcc-desktop` produces the host binary.
2. `setup-apple-extension.sh` (TBD) runs `xcodebuild -target
   CrabccNotifyExt -configuration Release` to compile the Swift
   target into `.appex`, then nests it under the bundled `.app`'s
   `Contents/PlugIns/` and re-signs with `codesign --force --deep`.

Production distribution additionally requires the entire `.app` to be
signed with a Developer ID Application certificate and notarized.

## Why a separate dir, not a feature flag of `crates/crabcc-desktop`

Cargo crates can't produce `.appex` outputs. The Swift sources need
their own `Info.plist`, their own entitlements, and their own
`Sources/` tree. Co-locating them under `apps/` matches the existing
pattern (`apps/crabcc-telegram` is also a workspace-external Rust
binary with its own Cargo.toml + Dockerfile + Taskfile).
