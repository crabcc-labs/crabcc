// StickyFeatureTests.swift — TCA TestStore stub (issue #192 phase 0).
//
// Demonstrates the testability TCA buys us: every action is observable,
// every state transition is exhaustively asserted, and Effects are
// driven by a controlled clock. Phase 1 fills in real assertions when
// StickyFeature wraps the live StickyManager.

import ComposableArchitecture
import XCTest

@testable import Crabcc

@MainActor
final class StickyFeatureTests: XCTestCase {
    func testBodyChange() async {
        let store = TestStore(initialState: StickyFeature.State()) {
            StickyFeature()
        }

        await store.send(.bodyChanged("hello")) {
            $0.body = "hello"
        }
    }
}
