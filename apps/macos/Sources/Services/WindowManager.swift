// WindowManager.swift — NSWindow lifecycle for sticky-note popouts.
//
// SwiftUI's `WindowGroup` is awkward to drive from a TCA Reducer (the
// `openWindow` environment value isn't reachable outside SwiftUI
// view bodies). This client owns the NSWindow instances directly,
// mirroring the legacy StickyManager pattern but exposed as a TCA
// `@Dependency` so the Reducer can stay pure.
//
// Phase 4 of #192 swaps NSScrollView + NSTextView for SwiftUI hosted
// via NSHostingView (Lottie load-state animation, TCA state binding).
// Phase 1 keeps the AppKit content for parity with the legacy bundle.

import AppKit
import Dependencies
import Foundation

@MainActor
struct WindowManagerClient {
    var openSticky: @MainActor (_ id: UUID, _ title: String, _ body: String) -> Void
    var closeSticky: @MainActor (_ id: UUID) -> Void
}

@MainActor
final class StickyWindowRegistry {
    static let shared = StickyWindowRegistry()
    private var windows: [UUID: NSWindow] = [:]
    private var delegateBox: [UUID: WindowDelegate] = [:]

    func open(id: UUID, title: String, body: String) {
        if let existing = windows[id] {
            existing.makeKeyAndOrderFront(nil)
            return
        }
        let frame = NSRect(x: 0, y: 0, width: 600, height: 400)
        let style: NSWindow.StyleMask = [.titled, .closable, .resizable, .miniaturizable]
        let w = NSWindow(
            contentRect: frame,
            styleMask: style,
            backing: .buffered,
            defer: false
        )
        w.title = title
        w.isReleasedWhenClosed = false
        w.level = .floating
        w.collectionBehavior = [.canJoinAllSpaces, .stationary]
        w.center()

        let scroll = NSScrollView(frame: frame)
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.borderType = .noBorder
        scroll.autoresizingMask = [.width, .height]

        let text = NSTextView(frame: frame)
        text.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        text.isEditable = false
        text.isSelectable = true
        text.isRichText = false
        text.allowsUndo = false
        text.string = body
        text.textContainerInset = NSSize(width: 8, height: 8)
        text.autoresizingMask = [.width]
        text.minSize = NSSize(width: 0, height: 0)
        text.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        text.isVerticallyResizable = true
        text.isHorizontallyResizable = false
        text.textContainer?.containerSize = NSSize(
            width: frame.width,
            height: CGFloat.greatestFiniteMagnitude
        )
        text.textContainer?.widthTracksTextView = true

        scroll.documentView = text
        w.contentView = scroll

        let delegate = WindowDelegate(id: id) { [weak self] closedID in
            self?.windows.removeValue(forKey: closedID)
            self?.delegateBox.removeValue(forKey: closedID)
        }
        w.delegate = delegate
        delegateBox[id] = delegate

        NSApp.activate(ignoringOtherApps: true)
        w.makeKeyAndOrderFront(nil)
        windows[id] = w
    }

    func close(id: UUID) {
        windows[id]?.close()
    }
}

private final class WindowDelegate: NSObject, NSWindowDelegate {
    let id: UUID
    let onClose: (UUID) -> Void

    init(id: UUID, onClose: @escaping (UUID) -> Void) {
        self.id = id
        self.onClose = onClose
    }

    func windowWillClose(_ notification: Notification) {
        onClose(id)
    }
}

extension WindowManagerClient: DependencyKey {
    static let liveValue = WindowManagerClient(
        openSticky: { id, title, body in
            StickyWindowRegistry.shared.open(id: id, title: title, body: body)
        },
        closeSticky: { id in
            StickyWindowRegistry.shared.close(id: id)
        }
    )

    static let testValue = WindowManagerClient(
        openSticky: { _, _, _ in },
        closeSticky: { _ in }
    )
}

extension DependencyValues {
    var windowManager: WindowManagerClient {
        get { self[WindowManagerClient.self] }
        set { self[WindowManagerClient.self] = newValue }
    }
}
