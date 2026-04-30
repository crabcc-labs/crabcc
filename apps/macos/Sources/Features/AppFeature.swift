// AppFeature.swift — top-level Reducer composing the menubar state.
//
// Phase 1 port of the entire AppDelegate.rebuildMenu() surface from the
// legacy installer/Crabcc.app/Contents/MacOS/menubar.swift. Each
// menu-section's data lives in this single state struct; SwiftUI views
// render directly off `@ObservableState`. Refresh fires on
// `menuOpened` (mirrors the legacy `menuNeedsUpdate` callback).

import AppKit
import ComposableArchitecture
import Foundation

@Reducer
struct AppFeature {
    @ObservableState
    struct State: Equatable {
        var version: String = ""
        var activeRepo: String? = nil
        var taskNames: [String] = []
        var processCounts: ProcessCounts = .init()
        var scheduledTasks: [ScheduledTask] = []
        var killEvents: [KillEvent] = []
        var telegramBot: TelegramBotState? = nil
        var sticky: StickyFeature.State = .init()
        var uninstallTelegramAlert: AlertState? = nil

        struct AlertState: Equatable {
            var title: String
            var message: String
        }
    }

    enum Action {
        case onAppear
        case menuOpened
        case stateRefreshed(StateSnapshot)
        case pickRepo
        case repoPicked(String?)
        case runTask(String)
        case reindexNow
        case runGuardNow
        case openLogs
        case reinstall
        case showAbout
        case forceQuit
        // launch agents
        case kickstart(String)
        case openKillLog(String)
        // telegram
        case telegramOpenMiniApp
        case telegramRestart
        case telegramOpenLog
        case telegramRevealPlist
        case telegramConfirmUninstall
        case telegramAlertDismissed
        case telegramUninstallConfirmed
        // sticky (delegated)
        case sticky(StickyFeature.Action)
    }

    struct StateSnapshot: Equatable {
        var activeRepo: String?
        var taskNames: [String]
        var counts: ProcessCounts
        var scheduledTasks: [ScheduledTask]
        var killEvents: [KillEvent]
        var telegramBot: TelegramBotState?
    }

    @Dependency(\.shell) var shell
    @Dependency(\.telemetry) var telemetry
    @Dependency(\.launchAgent) var launchAgent
    @Dependency(\.agentRuns) var agentRuns
    @Dependency(\.repo) var repoClient

    var body: some ReducerOf<Self> {
        Scope(state: \.sticky, action: \.sticky) {
            StickyFeature()
        }

        Reduce { state, action in
            switch action {
            case .onAppear:
                state.version =
                    (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String)
                    ?? "dev"
                telemetry.emit("app_launched", ["version": state.version])
                return .send(.menuOpened)

            case .menuOpened:
                telemetry.emit("menu_opened", [:])
                return .run { send in
                    let snap = await refresh()
                    await send(.stateRefreshed(snap))
                }

            case let .stateRefreshed(snap):
                state.activeRepo = snap.activeRepo
                state.taskNames = snap.taskNames
                state.processCounts = snap.counts
                state.scheduledTasks = snap.scheduledTasks
                state.killEvents = snap.killEvents
                state.telegramBot = snap.telegramBot
                return .none

            case .pickRepo:
                return .run { send in
                    let path = await pickRepoModal()
                    await send(.repoPicked(path))
                }

            case let .repoPicked(path):
                guard let p = path else { return .none }
                repoClient.setCurrent(p)
                return .send(.menuOpened)

            case let .runTask(name):
                guard let repo = state.activeRepo else { return .none }
                telemetry.emit("task_run", ["repo": repo, "task": name])
                runInTerminal(repo: repo, command: "task \(name)")
                return .none

            case .reindexNow:
                guard let repo = state.activeRepo else { return .none }
                telemetry.emit("reindex_invoked", ["repo": repo])
                runInTerminal(repo: repo, command: "crabcc index")
                return .none

            case .runGuardNow:
                telemetry.emit("guard_invoked", [:])
                let crabcc = NSString(string: "~/.cargo/bin/crabcc").expandingTildeInPath
                shell.runDetached([crabcc, "agent-guard", "--json"])
                return .none

            case .openLogs:
                let dir = NSString(string: "~/Library/Logs/Crabcc").expandingTildeInPath
                try? FileManager.default.createDirectory(
                    atPath: dir, withIntermediateDirectories: true
                )
                shell.runDetached(["/usr/bin/open", dir])
                return .none

            case .reinstall:
                telemetry.emit("reinstall_invoked", [:])
                let updater = Bundle.main.resourceURL?
                    .appendingPathComponent("scripts/update.sh").path ?? ""
                let cmd = "bash \(updater.replacingOccurrences(of: "\"", with: "\\\""))"
                let log = NSString(string: "~/Library/Logs/Crabcc/installer.log")
                    .expandingTildeInPath
                let script = """
                tell application "Terminal"
                    activate
                    do script "\(cmd) 2>&1 | tee \(log)"
                end tell
                """
                shell.runDetached(["/usr/bin/osascript", "-e", script])
                return .none

            case .showAbout:
                showAboutAlert(version: state.version, shell: shell)
                return .none

            case .forceQuit:
                telemetry.emit("force_quit", [:])
                exit(0)

            case let .kickstart(label):
                telemetry.emit("launchagent_kickstart", ["label": label])
                launchAgent.kickstart(label)
                return .none

            case let .openKillLog(runId):
                telemetry.emit("kill_log_opened", ["run_id": runId])
                let p = NSString(
                    string: "~/.crabcc/agents/\(runId)/.agent-\(runId)-kill-log"
                ).expandingTildeInPath
                if FileManager.default.fileExists(atPath: p) {
                    shell.runDetached(["/usr/bin/open", "-t", p])
                } else {
                    let dir = NSString(string: "~/.crabcc/agents/\(runId)")
                        .expandingTildeInPath
                    shell.runDetached(["/usr/bin/open", dir])
                }
                return .none

            case .telegramOpenMiniApp:
                telemetry.emit("telegram_bot.open_mini_app", [:])
                if let url = URL(string: "http://localhost:8090/?role=mini") {
                    NSWorkspace.shared.open(url)
                }
                return .none

            case .telegramRestart:
                telemetry.emit("telegram_bot.restart", [:])
                launchAgent.kickstart("com.crabcc.telegram-bot")
                return .none

            case .telegramOpenLog:
                telemetry.emit("telegram_bot.view_log", [:])
                let outLog = NSString(
                    string: "~/Library/Logs/Crabcc/telegram-bot.out.log"
                ).expandingTildeInPath
                let errLog = NSString(
                    string: "~/Library/Logs/Crabcc/telegram-bot.err.log"
                ).expandingTildeInPath
                var paths: [String] = []
                if FileManager.default.fileExists(atPath: outLog) { paths.append(outLog) }
                if FileManager.default.fileExists(atPath: errLog) { paths.append(errLog) }
                if paths.isEmpty {
                    shell.runDetached([
                        "/usr/bin/open",
                        NSString(string: "~/Library/Logs/Crabcc")
                            .expandingTildeInPath,
                    ])
                } else {
                    shell.runDetached(["/usr/bin/open", "-a", "Console"] + paths)
                }
                return .none

            case .telegramRevealPlist:
                telemetry.emit("telegram_bot.reveal_plist", [:])
                let p = NSString(
                    string: "~/Library/LaunchAgents/com.crabcc.telegram-bot.plist"
                ).expandingTildeInPath
                shell.runDetached(["/usr/bin/open", "-R", p])
                return .none

            case .telegramConfirmUninstall:
                state.uninstallTelegramAlert = .init(
                    title: "Uninstall Telegram Bot service?",
                    message: """
                    Removes the LaunchAgent and stops the bot.
                    Your .env and config remain. Reinstall any time with:
                        crabcc-telegram install-service
                    """
                )
                return .none

            case .telegramAlertDismissed:
                state.uninstallTelegramAlert = nil
                return .none

            case .telegramUninstallConfirmed:
                state.uninstallTelegramAlert = nil
                telemetry.emit("telegram_bot.uninstall_invoked", [:])
                let bin = NSString(string: "~/.cargo/bin/crabcc-telegram")
                    .expandingTildeInPath
                if FileManager.default.fileExists(atPath: bin) {
                    shell.runDetached([bin, "uninstall-service"])
                } else {
                    launchAgent.bootout("com.crabcc.telegram-bot")
                }
                return .none

            case .sticky:
                return .none
            }
        }
    }

    // MARK: - effect helpers

    private func refresh() async -> StateSnapshot {
        let activeRepo = repoClient.current()
        let taskNames = activeRepo.map { repoClient.parseTaskfile($0) } ?? []
        var counts = ProcessCounts()
        counts.indexes = shell.pgrepCount("crabcc index")
        counts.watches = shell.pgrepCount("crabcc watch")
        counts.agents = shell.pgrepCount("crabcc agent") + agentRuns.activeRunCount()
        counts.agentd = shell.pgrepCount("crabcc-agentd")
        let scheduled = launchAgent.scheduledTasks()
        let kills = agentRuns.recentKillEvents(10)
        let bot = launchAgent.telegramBotState()
        return StateSnapshot(
            activeRepo: activeRepo,
            taskNames: taskNames,
            counts: counts,
            scheduledTasks: scheduled,
            killEvents: kills,
            telegramBot: bot
        )
    }
}

// MARK: - shell-bridge helpers

func runInTerminal(repo: String, command: String) {
    let escaped = command.replacingOccurrences(of: "\\", with: "\\\\")
        .replacingOccurrences(of: "\"", with: "\\\"")
    let escRepo = repo.replacingOccurrences(of: "\"", with: "\\\"")
    let script = """
    tell application "Terminal"
        activate
        do script "cd \\"\(escRepo)\\" && \(escaped)"
    end tell
    """
    @Dependency(\.shell) var shell
    shell.runDetached(["/usr/bin/osascript", "-e", script])
}

@MainActor
func pickRepoModal() async -> String? {
    await withCheckedContinuation { continuation in
        DispatchQueue.main.async {
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.allowsMultipleSelection = false
            panel.title = "Pick a crabcc repo"
            if panel.runModal() == .OK, let url = panel.url {
                continuation.resume(returning: url.path)
            } else {
                continuation.resume(returning: nil)
            }
        }
    }
}

@MainActor
func showAboutAlert(version: String, shell: ShellClient) {
    let id = (Bundle.main.infoDictionary?["CFBundleIdentifier"] as? String) ?? ""
    let bundlePath = Bundle.main.bundlePath
    let cliVersion = shell.captureStdout([
        NSString(string: "~/.cargo/bin/crabcc").expandingTildeInPath, "--version",
    ]).trimmingCharacters(in: .whitespacesAndNewlines)
    let agentdLoaded = shell.captureStdout([
        "/bin/launchctl", "print", "gui/\(getuid())/com.crabcc.agentd",
    ]).contains("state = running")

    let alert = NSAlert()
    alert.messageText = "Crabcc \(version)"
    alert.informativeText = """
    Identifier: \(id)
    Bundle:     \(bundlePath)
    CLI:        \(cliVersion.isEmpty ? "(crabcc not on PATH)" : cliVersion)
    agentd:     \(agentdLoaded ? "running (LaunchAgent)" : "not loaded")
    """
    alert.alertStyle = .informational
    alert.addButton(withTitle: "Open Repo Page")
    alert.addButton(withTitle: "Close")
    NSApp.activate(ignoringOtherApps: true)
    alert.window.level = .floating
    if alert.runModal() == .alertFirstButtonReturn,
       let url = URL(string: "https://github.com/peterlodri-sec/crabcc") {
        NSWorkspace.shared.open(url)
    }
}
