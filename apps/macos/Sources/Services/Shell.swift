// Shell.swift — Process / launchctl / pgrep helpers.
//
// Direct port of the global functions in the legacy menubar.swift:
// runDetached, captureStdout, pgrepCount. Wrapped in TCA `@Dependency`
// for testability (TestStore replaces this with a stub).

import Dependencies
import Foundation

struct ShellClient {
    var runDetached: @Sendable (_ argv: [String]) -> Void
    var captureStdout: @Sendable (_ argv: [String]) -> String
    var pgrepCount: @Sendable (_ pattern: String) -> Int
}

extension ShellClient: DependencyKey {
    static let liveValue = ShellClient(
        runDetached: { argv in
            guard !argv.isEmpty else { return }
            let p = Process()
            p.executableURL = URL(fileURLWithPath: argv[0])
            p.arguments = Array(argv.dropFirst())
            try? p.run()
        },
        captureStdout: { argv in
            guard !argv.isEmpty else { return "" }
            let p = Process()
            p.executableURL = URL(fileURLWithPath: argv[0])
            p.arguments = Array(argv.dropFirst())
            let out = Pipe()
            p.standardOutput = out
            p.standardError = Pipe()
            do {
                try p.run()
                p.waitUntilExit()
            } catch {
                return ""
            }
            let data = out.fileHandleForReading.readDataToEndOfFile()
            return String(data: data, encoding: .utf8) ?? ""
        },
        pgrepCount: { pattern in
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
            p.arguments = ["-f", "-c", pattern]
            let out = Pipe()
            p.standardOutput = out
            p.standardError = Pipe()
            do {
                try p.run()
                p.waitUntilExit()
            } catch {
                return 0
            }
            let raw = String(
                data: out.fileHandleForReading.readDataToEndOfFile(),
                encoding: .utf8
            ) ?? ""
            return Int(raw.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 0
        }
    )

    static let testValue = ShellClient(
        runDetached: { _ in },
        captureStdout: { _ in "" },
        pgrepCount: { _ in 0 }
    )
}

extension DependencyValues {
    var shell: ShellClient {
        get { self[ShellClient.self] }
        set { self[ShellClient.self] = newValue }
    }
}
