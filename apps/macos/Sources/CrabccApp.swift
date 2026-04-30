// CrabccApp.swift — Tuist-built entry point (issue #192 phase 0).
//
// Minimal SwiftUI `MenuBarExtra` app that proves the Tuist build
// pipeline produces a working .app. NOT a replacement for the
// production menubar yet — that lives at
// `installer/Crabcc.app/Contents/MacOS/menubar.swift` and stays
// untouched through Phase 0. Phase 3 of the #192 migration is when
// the SwiftUI MenuBarExtra replaces the AppKit NSStatusItem shim.
//
// `MenuBarExtra` is macOS 13+ — matches our deployment target.

import AppKit
import SwiftUI

@main
struct CrabccApp: App {
    var body: some Scene {
        MenuBarExtra("Crabcc", systemImage: "shippingbox.fill") {
            VStack(alignment: .leading, spacing: 4) {
                Text("Crabcc — Tuist scaffold")
                    .font(.system(.body, design: .monospaced))
                Text("Phase 0 of issue #192 — TCA + Tuist + Lottie + AsyncQueue")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(8)

            Divider()

            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
            .keyboardShortcut("q")
        }
        .menuBarExtraStyle(.menu)
    }
}
