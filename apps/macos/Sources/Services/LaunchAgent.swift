// LaunchAgent.swift — launchctl + LaunchAgents directory queries.
//
// Direct port of the legacy menubar.swift `scheduledTasks()`,
// `telegramBotState()`, and the kickstart / bootout helpers.

import Dependencies
import Foundation

struct LaunchAgentClient {
    var scheduledTasks: @Sendable () -> [ScheduledTask]
    var telegramBotState: @Sendable () -> TelegramBotState?
    var kickstart: @Sendable (_ label: String) -> Void
    var bootout: @Sendable (_ label: String) -> Void
}

extension LaunchAgentClient: DependencyKey {
    static let liveValue: LaunchAgentClient = {
        @Dependency(\.shell) var shell

        return LaunchAgentClient(
            scheduledTasks: {
                let dir = NSString(string: "~/Library/LaunchAgents")
                    .expandingTildeInPath
                guard let entries = try? FileManager.default.contentsOfDirectory(atPath: dir)
                else { return [] }
                let labels = entries
                    .filter { $0.hasPrefix("com.crabcc.") && $0.hasSuffix(".plist") }
                    .map { String($0.dropLast(".plist".count)) }
                    .sorted()
                let uid = getuid()
                return labels.map { label -> ScheduledTask in
                    let body = shell.captureStdout([
                        "/bin/launchctl", "print", "gui/\(uid)/\(label)"
                    ])
                    let pid: Int? = body
                        .split(separator: "\n")
                        .first(where: { $0.contains("pid =") })
                        .flatMap { line in
                            Int(String(line).split(separator: "=").last?
                                .trimmingCharacters(in: .whitespaces) ?? "")
                        }
                    let lastExit: Int? = body
                        .split(separator: "\n")
                        .first(where: { $0.contains("last exit code =") })
                        .flatMap { line in
                            Int(String(line).split(separator: "=").last?
                                .trimmingCharacters(in: .whitespaces) ?? "")
                        }
                    let plistPath = "\(dir)/\(label).plist"
                    let plistBody =
                        (try? String(contentsOfFile: plistPath, encoding: .utf8)) ?? ""
                    let cadence = humanizeCadence(plist: plistBody)
                    return ScheduledTask(
                        label: label,
                        cadence: cadence,
                        lastExitCode: lastExit,
                        pid: pid
                    )
                }
            },
            telegramBotState: {
                let plistPath = NSString(
                    string: "~/Library/LaunchAgents/com.crabcc.telegram-bot.plist"
                ).expandingTildeInPath
                guard FileManager.default.fileExists(atPath: plistPath) else {
                    return nil
                }
                let uid = getuid()
                let body = shell.captureStdout([
                    "/bin/launchctl", "print", "gui/\(uid)/com.crabcc.telegram-bot"
                ])
                let pid: Int? = body
                    .split(separator: "\n")
                    .first(where: { $0.contains("pid =") })
                    .flatMap { line in
                        Int(String(line).split(separator: "=").last?
                            .trimmingCharacters(in: .whitespaces) ?? "")
                    }
                let lastExit: Int? = body
                    .split(separator: "\n")
                    .first(where: { $0.contains("last exit code =") })
                    .flatMap { line in
                        Int(String(line).split(separator: "=").last?
                            .trimmingCharacters(in: .whitespaces) ?? "")
                    }
                let uptime: Int? = pid.flatMap { p -> Int? in
                    let raw = shell.captureStdout([
                        "/bin/ps", "-o", "etime=", "-p", String(p)
                    ]).trimmingCharacters(in: .whitespacesAndNewlines)
                    return parseEtime(raw)
                }
                return TelegramBotState(
                    pid: pid,
                    lastExitCode: lastExit,
                    uptimeSeconds: uptime
                )
            },
            kickstart: { label in
                let uid = getuid()
                shell.runDetached([
                    "/bin/launchctl", "kickstart", "-k", "gui/\(uid)/\(label)"
                ])
            },
            bootout: { label in
                let uid = getuid()
                shell.runDetached([
                    "/bin/launchctl", "bootout", "gui/\(uid)/\(label)"
                ])
            }
        )
    }()

    static let testValue = LaunchAgentClient(
        scheduledTasks: { [] },
        telegramBotState: { nil },
        kickstart: { _ in },
        bootout: { _ in }
    )
}

extension DependencyValues {
    var launchAgent: LaunchAgentClient {
        get { self[LaunchAgentClient.self] }
        set { self[LaunchAgentClient.self] = newValue }
    }
}

// MARK: - shared parsers

func humanizeCadence(plist body: String) -> String {
    if let interval = matchInteger(body: body, key: "StartInterval") {
        return humanizeInterval(interval)
    }
    if body.contains("<key>KeepAlive</key>") { return "kept alive" }
    if body.contains("<key>RunAtLoad</key>") { return "at login" }
    return "manual"
}

func matchInteger(body: String, key: String) -> Int? {
    guard let keyRange = body.range(of: "<key>\(key)</key>") else { return nil }
    let tail = body[keyRange.upperBound...]
    guard let intStart = tail.range(of: "<integer>"),
          let intEnd = tail.range(
            of: "</integer>",
            range: intStart.upperBound..<tail.endIndex
          )
    else { return nil }
    return Int(tail[intStart.upperBound..<intEnd.lowerBound])
}

func humanizeInterval(_ secs: Int) -> String {
    if secs < 60 { return "every \(secs)s" }
    if secs < 3600 { return "every \(secs / 60) min" }
    return "every \(secs / 3600) h"
}

func parseEtime(_ s: String) -> Int? {
    var rest = s
    var days = 0
    if let dashIdx = rest.firstIndex(of: "-") {
        days = Int(rest[..<dashIdx]) ?? 0
        rest = String(rest[rest.index(after: dashIdx)...])
    }
    let parts = rest.split(separator: ":").compactMap { Int($0) }
    switch parts.count {
    case 3: return days * 86400 + parts[0] * 3600 + parts[1] * 60 + parts[2]
    case 2: return days * 86400 + parts[0] * 60 + parts[1]
    case 1: return days * 86400 + parts[0]
    default: return nil
    }
}

func humanizeUptime(_ secs: Int) -> String {
    if secs < 60 { return "\(secs)s up" }
    if secs < 3600 { return "\(secs / 60)m up" }
    if secs < 86400 { return "\(secs / 3600)h up" }
    return "\(secs / 86400)d up"
}
