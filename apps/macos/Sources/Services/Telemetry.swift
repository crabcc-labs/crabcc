// Telemetry.swift — JSON-lines event log to ~/Library/Logs/Crabcc/menubar.events.jsonl
//
// Direct port of the global `emitEvent` from the legacy menubar.swift.
// Cheap (one fopen-append-fclose per call). Wrapped in `@Dependency`
// so tests inject a no-op or capture-to-array variant.

import Dependencies
import Foundation

struct TelemetryClient {
    var emit: @Sendable (_ event: String, _ props: [String: any Sendable]) -> Void
}

extension TelemetryClient: DependencyKey {
    static let liveValue = TelemetryClient { event, props in
        let path = NSString("~/Library/Logs/Crabcc/menubar.events.jsonl")
            .expandingTildeInPath
        let dir = (path as NSString).deletingLastPathComponent
        try? FileManager.default.createDirectory(
            atPath: dir,
            withIntermediateDirectories: true
        )

        let ts = ISO8601DateFormatter().string(from: Date())
        var rec: [String: Any] = ["ts": ts, "event": event, "pid": getpid()]
        for (k, v) in props { rec[k] = v }

        guard let data = try? JSONSerialization.data(withJSONObject: rec),
              var line = String(data: data, encoding: .utf8) else { return }
        line.append("\n")

        if let fh = FileHandle(forWritingAtPath: path) {
            defer { try? fh.close() }
            _ = try? fh.seekToEnd()
            try? fh.write(contentsOf: Data(line.utf8))
        } else {
            try? line.write(toFile: path, atomically: true, encoding: .utf8)
        }
    }

    static let testValue = TelemetryClient { _, _ in }
}

extension DependencyValues {
    var telemetry: TelemetryClient {
        get { self[TelemetryClient.self] }
        set { self[TelemetryClient.self] = newValue }
    }
}
