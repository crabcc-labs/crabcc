// AgentRuns.swift — singleton SQLite reads from ~/.crabcc/_internal.db.
//
// Direct port of `activeAgentRunCount`, `activeAgentRunCountFromDb`,
// `recentKillEvents` from the legacy menubar.swift. Shells out to
// /usr/bin/sqlite3 in -readonly mode (no FFI / no SQLite linkage in
// the app — keeps the bundle small and avoids a custom build setup).

import Dependencies
import Foundation

struct AgentRunsClient {
    var activeRunCount: @Sendable () -> Int
    var recentKillEvents: @Sendable (_ limit: Int) -> [KillEvent]
}

extension AgentRunsClient: DependencyKey {
    static let liveValue: AgentRunsClient = {
        @Dependency(\.shell) var shell
        let dbPath = NSString(string: "~/.crabcc/_internal.db").expandingTildeInPath

        return AgentRunsClient(
            activeRunCount: {
                if FileManager.default.fileExists(atPath: dbPath) {
                    let s = shell.captureStdout([
                        "/usr/bin/sqlite3", "-readonly", dbPath,
                        "SELECT count(*) FROM agent_runs WHERE status='running';"
                    ]).trimmingCharacters(in: .whitespacesAndNewlines)
                    if let n = Int(s) { return n }
                }
                // Fallback: walk ~/.crabcc/agents/<id>/lock
                let runsRoot = NSString(string: "~/.crabcc/agents").expandingTildeInPath
                guard let entries = try? FileManager.default.contentsOfDirectory(atPath: runsRoot)
                else { return 0 }
                return entries.filter {
                    FileManager.default.fileExists(atPath: "\(runsRoot)/\($0)/lock")
                }.count
            },
            recentKillEvents: { limit in
                guard FileManager.default.fileExists(atPath: dbPath) else { return [] }
                let raw = shell.captureStdout([
                    "/usr/bin/sqlite3", "-readonly", dbPath,
                    """
                    SELECT run_id || '|' || reason || '|' || COALESCE(detail,'')
                    FROM agent_kill_events
                    ORDER BY killed_at DESC
                    LIMIT \(limit);
                    """
                ])
                return raw.split(separator: "\n").compactMap { line in
                    let parts = line.split(
                        separator: "|", maxSplits: 2, omittingEmptySubsequences: false
                    )
                    guard parts.count == 3 else { return nil }
                    return KillEvent(
                        runId: String(parts[0]),
                        reason: String(parts[1]),
                        detail: String(parts[2])
                    )
                }
            }
        )
    }()

    static let testValue = AgentRunsClient(
        activeRunCount: { 0 },
        recentKillEvents: { _ in [] }
    )
}

extension DependencyValues {
    var agentRuns: AgentRunsClient {
        get { self[AgentRunsClient.self] }
        set { self[AgentRunsClient.self] = newValue }
    }
}
