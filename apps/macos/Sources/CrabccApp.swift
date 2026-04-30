// CrabccApp.swift — production entry point.
//
// SwiftUI MenuBarExtra hosts the entire menu surface. Replaces the
// hand-rolled NSStatusItem shim from the legacy menubar.swift.
// `LSUIElement = true` (in Info.plist) keeps the app off the dock.

import AppKit
import ComposableArchitecture
import SwiftUI

@main
struct CrabccApp: App {
    @State private var store = Store(initialState: AppFeature.State()) {
        AppFeature()
    }

    var body: some Scene {
        MenuBarExtra {
            MenuBarView(store: store)
        } label: {
            Image(systemName: "shippingbox.fill")
        }
        .menuBarExtraStyle(.menu)
    }
}
