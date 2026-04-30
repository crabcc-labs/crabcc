// StickyFeature.swift — TCA Reducer scaffolding (issue #192 phase 0).
//
// Phase 0 proof-of-life: imports all four declared SPM dependencies so
// `tuist generate` + `xcodebuild build` proves the dependency graph
// resolves and links. Real wiring into the live menubar happens in
// later phases per the migration plan in #192:
// - Phase 1: this Reducer wraps the existing StickyManager from #190
// - Phase 4: Lottie animation for sticky-window load state
// - Phase 5: ActorQueue serializing socket reads from #189 Phase 1

import AsyncQueue
import ComposableArchitecture
import Foundation
import Lottie

@Reducer
struct StickyFeature {
    @ObservableState
    struct State: Equatable {
        var title: String = ""
        var body: String = ""
        var isLoading: Bool = false
    }

    enum Action: Equatable {
        case openFromClipboard
        case clipboardLoaded(String)
        case bodyChanged(String)
    }

    var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .openFromClipboard:
                state.isLoading = true
                state.title = "Sticky " + ISO8601DateFormatter().string(from: Date())
                return .run { send in
                    // Real clipboard read lands in Phase 1; this is the
                    // shape the Effect will take. NSPasteboard access
                    // moves into a dependency injected via TCA's
                    // `@Dependency` macro.
                    await send(.clipboardLoaded(""))
                }
            case let .clipboardLoaded(body):
                state.body = body
                state.isLoading = false
                return .none
            case let .bodyChanged(body):
                state.body = body
                return .none
            }
        }
    }
}

// MARK: - linker reachability probes
//
// These force the SPM products to be retained in the final binary so
// Phase 0 verifies link-time, not just compile-time, dependency
// resolution. Real Lottie / AsyncQueue usage replaces these probes
// per the migration plan.

@MainActor
enum LibraryReachability {
    static let lottieView: LottieAnimationView.Type = LottieAnimationView.self
    static let asyncQueue: AsyncQueue.Type = AsyncQueue.self
}
