// StickyFeature.swift — TCA Reducer for sticky-note windows.
//
// Phase 1 port of the legacy installer/Crabcc.app/Contents/MacOS/sticky.swift.
// State holds an IdentifiedArray of open stickies; opening one creates an
// NSWindow via WindowManagerClient. Window close fires `.dismissed(id)`
// back into the reducer so state stays in sync.

import AppKit
import ComposableArchitecture
import Foundation

@Reducer
struct StickyFeature {
    @ObservableState
    struct Sticky: Equatable, Identifiable {
        let id: UUID
        var title: String
        var body: String
    }

    @ObservableState
    struct State: Equatable {
        var stickies: IdentifiedArrayOf<Sticky> = []
    }

    enum Action: Equatable {
        case newFromClipboard
        case open(title: String, body: String)
        case dismissed(id: UUID)
    }

    @Dependency(\.windowManager) var windowManager
    @Dependency(\.telemetry) var telemetry

    var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .newFromClipboard:
                let body = NSPasteboard.general.string(forType: .string) ?? ""
                let title = "Sticky " + Self.timestamp()
                telemetry.emit("sticky.new_from_clipboard", [:])
                return .send(.open(title: title, body: body))

            case let .open(title, body):
                let id = UUID()
                state.stickies.append(Sticky(id: id, title: title, body: body))
                telemetry.emit("sticky.opened", [
                    "title_len": title.count,
                    "body_len": body.count,
                ])
                return .run { _ in
                    await MainActor.run {
                        windowManager.openSticky(id, title, body)
                    }
                }

            case let .dismissed(id):
                state.stickies.remove(id: id)
                telemetry.emit("sticky.closed", [:])
                return .none
            }
        }
    }

    private static func timestamp() -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return f.string(from: Date())
    }
}
