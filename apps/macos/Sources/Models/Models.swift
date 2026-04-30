// Models.swift — value types shared across features.
//
// Plain Swift `Equatable` structs, no AppKit/SwiftUI imports. Mirror
// the data shapes the legacy menubar.swift exposed via `pgrepCount`,
// `scheduledTasks`, `recentKillEvents`, and `telegramBotState` —
// kept identical so the SwiftUI menu renders the same content.

import Foundation

struct ProcessCounts: Equatable {
    var indexes: Int = 0
    var watches: Int = 0
    var agents: Int = 0
    var agentd: Int = 0
}

struct ScheduledTask: Equatable, Identifiable {
    var id: String { label }
    let label: String
    let cadence: String
    let lastExitCode: Int?
    let pid: Int?
    var isRunning: Bool { pid != nil }
}

struct KillEvent: Equatable, Identifiable {
    var id: String { runId }
    let runId: String
    let reason: String
    let detail: String
}

struct TelegramBotState: Equatable {
    let pid: Int?
    let lastExitCode: Int?
    let uptimeSeconds: Int?

    var isRunning: Bool { pid != nil }
}
