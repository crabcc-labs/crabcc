// sticky.swift — sticky-note windows for long agent outputs (issue #189 phase 0).
//
// Compiled at build time alongside menubar.swift by scripts/build-dmg.sh:
//   swiftc -O -o Crabcc menubar.swift sticky.swift
//
// AppKit-only (NSWindow + NSScrollView + NSTextView). No SwiftUI, no .xib —
// matches the single-file ethos of menubar.swift. Phase 0 covers only
// "New Sticky from Clipboard" → floating 600x400 window with monospaced
// read-only text. CLI bridge, --tail streaming, persistence, URL scheme,
// and Markdown rendering land in later phases (see issue #189).

import AppKit
import Foundation

// MARK: - sticky manager

// Holds strong refs to every open sticky so they're not deallocated when
// the creating scope exits. `windowWillClose` drops the ref so closed
// windows are GC'd. `isReleasedWhenClosed = false` lives on each NSWindow
// to prevent AppKit's default release-on-close (which would crash, since
// we'd then be holding a dangling pointer).
final class StickyManager: NSObject, NSWindowDelegate {
    static let shared = StickyManager()

    private var windows: [NSWindow] = []

    // Public entry point used by the menubar. Reads NSPasteboard `.string`,
    // falls back to "" when the clipboard has no text (e.g. an image),
    // and opens an empty sticky in that case rather than silently no-op-ing
    // — empty stickies are a valid surface for paste-later workflows.
    func openFromClipboard() {
        let body = NSPasteboard.general.string(forType: .string) ?? ""
        open(title: "Sticky " + Self.timestamp(), body: body)
    }

    func open(title: String, body: String) {
        let w = makeWindow(title: title, body: body)
        w.delegate = self
        windows.append(w)
        NSApp.activate(ignoringOtherApps: true)
        w.makeKeyAndOrderFront(nil)
        emitEvent("sticky.opened", [
            "title_len": title.count,
            "body_len": body.count,
        ])
    }

    func windowWillClose(_ notification: Notification) {
        guard let w = notification.object as? NSWindow else { return }
        emitEvent("sticky.closed", [:])
        windows.removeAll { $0 === w }
    }

    // MARK: - window factory

    private func makeWindow(title: String, body: String) -> NSWindow {
        let frame = NSRect(x: 0, y: 0, width: 600, height: 400)
        let style: NSWindow.StyleMask = [.titled, .closable, .resizable, .miniaturizable]
        let w = NSWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: false)
        w.title = title
        w.isReleasedWhenClosed = false
        w.level = .floating
        w.collectionBehavior = [.canJoinAllSpaces, .stationary]
        w.center()

        // NSScrollView + NSTextView built programmatically — no .xib, same
        // pattern as menubar.swift.
        let scroll = NSScrollView(frame: frame)
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = false
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
        text.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude,
                              height: CGFloat.greatestFiniteMagnitude)
        text.isVerticallyResizable = true
        text.isHorizontallyResizable = false
        text.textContainer?.containerSize = NSSize(width: frame.width,
                                                   height: CGFloat.greatestFiniteMagnitude)
        text.textContainer?.widthTracksTextView = true

        scroll.documentView = text
        w.contentView = scroll
        return w
    }

    private static func timestamp() -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return f.string(from: Date())
    }
}
