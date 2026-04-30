// MenuBarView.swift — SwiftUI menu hierarchy hosted by MenuBarExtra.
//
// Phase 1 port of AppDelegate.rebuildMenu() from the legacy
// menubar.swift. Each section of the original NSMenu becomes a
// SwiftUI Section / Menu / Button — MenuBarExtra(.menu) renders
// these as native NSMenu items, so the visual result is identical
// to the legacy bundle.

import ComposableArchitecture
import SwiftUI

struct MenuBarView: View {
    @Bindable var store: StoreOf<AppFeature>

    var body: some View {
        Group {
            // Header — version + active repo.
            Text("Crabcc \(store.version)")
            Button(repoLabel) { store.send(.pickRepo) }

            Divider()

            // Live process counts (read-only labels).
            ForEach(counterRows, id: \.label) { row in
                Text(row.title).disabled(true)
            }

            Divider()

            // Run Task submenu.
            Menu("Run Task") {
                if store.activeRepo == nil {
                    Text("(pick a repo first)").disabled(true)
                } else if store.taskNames.isEmpty {
                    Text("(no tasks found in Taskfile.yml)").disabled(true)
                } else {
                    ForEach(store.taskNames, id: \.self) { name in
                        Button(name) { store.send(.runTask(name)) }
                    }
                }
            }

            // Telegram Bot top-level section (issue #156). Hidden when
            // the LaunchAgent plist is missing.
            if let bot = store.telegramBot {
                Menu(telegramHeaderTitle(bot)) {
                    Button("Open Mini App") { store.send(.telegramOpenMiniApp) }
                        .keyboardShortcut("m")
                    Button("Restart bot") { store.send(.telegramRestart) }
                        .keyboardShortcut("r")
                    Button("View bot log") { store.send(.telegramOpenLog) }
                    Button("Reveal plist in Finder") {
                        store.send(.telegramRevealPlist)
                    }
                    Divider()
                    Button("Uninstall service…") {
                        store.send(.telegramConfirmUninstall)
                    }
                }
            }

            // Scheduled Tasks (LaunchAgents).
            Menu("Scheduled Tasks (\(store.scheduledTasks.count))") {
                if store.scheduledTasks.isEmpty {
                    Text("(no LaunchAgents installed)").disabled(true)
                } else {
                    ForEach(store.scheduledTasks) { task in
                        Button(scheduledLabel(task)) {
                            store.send(.kickstart(task.label))
                        }
                    }
                }
            }

            // Recent Kills (from singleton SQLite db).
            Menu("Recent Kills (\(store.killEvents.count))") {
                if store.killEvents.isEmpty {
                    Text("(no kills recorded)").disabled(true)
                } else {
                    ForEach(store.killEvents) { kill in
                        Button(killLabel(kill)) {
                            store.send(.openKillLog(kill.runId))
                        }
                    }
                }
            }

            Divider()

            Button("Reindex Now") { store.send(.reindexNow) }
            Button("Run Guard Now") { store.send(.runGuardNow) }
            Button("New Sticky from Clipboard") {
                store.send(.sticky(.newFromClipboard))
            }.keyboardShortcut("n")
            Button("Open Logs") { store.send(.openLogs) }
            Button("Reinstall / Update…") { store.send(.reinstall) }

            Divider()

            Button("About Crabcc…") { store.send(.showAbout) }
            Button("Quit Crabcc") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
            Button("Force Quit (skip modals)") { store.send(.forceQuit) }
                .keyboardShortcut("Q")
        }
        .onAppear { store.send(.onAppear) }
    }

    // MARK: - derived labels

    private var repoLabel: String {
        if let r = store.activeRepo {
            return "Active repo: \((r as NSString).lastPathComponent)"
        }
        return "Active repo: (none)"
    }

    private var counterRows: [(label: String, title: String)] {
        let c = store.processCounts
        return [
            ("indexes", "indexes: \(c.indexes)" + (c.indexes > 0 ? "" : "  (idle)")),
            ("watches", "watches: \(c.watches)" + (c.watches > 0 ? "" : "  (idle)")),
            ("agents", "agents: \(c.agents)" + (c.agents > 0 ? "" : "  (idle)")),
            ("agentd", "agentd: \(c.agentd)" + (c.agentd > 0 ? "" : "  (idle)")),
        ]
    }

    private func scheduledLabel(_ t: ScheduledTask) -> String {
        let runState = t.isRunning ? "● running pid=\(t.pid ?? 0)" : "○ idle"
        let exitTail = t.lastExitCode.map { " · last_exit=\($0)" } ?? ""
        let stripped = t.label.replacingOccurrences(of: "com.crabcc.", with: "")
        return "\(stripped) · \(t.cadence) · \(runState)\(exitTail)"
    }

    private func killLabel(_ k: KillEvent) -> String {
        "\(k.runId.prefix(10))… · \(k.reason) · \(k.detail.prefix(50))"
    }
}

// MARK: - telegram header

func telegramHeaderTitle(_ bot: TelegramBotState) -> String {
    let prefix: String
    if bot.isRunning {
        prefix = "●"
    } else if let exit = bot.lastExitCode, exit != 0 {
        prefix = "◐"
    } else {
        prefix = "○"
    }
    var parts: [String] = ["\(prefix) Telegram Bot"]
    if bot.isRunning, let p = bot.pid {
        parts.append("running pid=\(p)")
    } else {
        parts.append("idle")
    }
    if let up = bot.uptimeSeconds, bot.isRunning {
        parts.append(humanizeUptime(up))
    }
    if let exit = bot.lastExitCode, exit != 0 {
        parts.append("last_exit=\(exit)")
    }
    return parts.joined(separator: " · ")
}
