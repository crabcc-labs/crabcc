// menubar.swift — single-file Swift entry point for Crabcc.app's menubar UI.
//
// Compiled at build time by scripts/build-dmg.sh:
//   swiftc -O -o Crabcc.app/Contents/MacOS/Crabcc menubar.swift
//
// Why not pure shell: macOS NSStatusItem is AppKit-only — no shell hook
// can drive a menubar entry. This file is the smallest possible AppKit
// shim (one class, no third-party deps). All work is delegated to the
// bundled shell helpers (crabcc-installer, crabcc-agentd) and external
// `crabcc` / `task` binaries.

import AppKit
import Foundation

// MARK: - state

let kActiveRepoPath  = NSString(string: "~/.crabcc/agent/active-repo").expandingTildeInPath
let kReposListPath   = NSString(string: "~/.crabcc/agent/repos.list").expandingTildeInPath
let kLogsDir         = NSString(string: "~/Library/Logs/Crabcc").expandingTildeInPath
let kEventsLogPath   = NSString(string: "~/Library/Logs/Crabcc/menubar.events.jsonl").expandingTildeInPath

// MARK: - telemetry
//
// Emit JSON-lines events parallel to the Rust crates' tracing-appender
// JSON file from issue #90. Cheap (one fopen-append-fclose per event)
// and safe to call from the main thread — events are short.

func emitEvent(_ name: String, _ props: [String: Any] = [:]) {
    let ts = ISO8601DateFormatter().string(from: Date())
    var rec: [String: Any] = ["ts": ts, "event": name, "pid": getpid()]
    for (k, v) in props { rec[k] = v }
    guard let data = try? JSONSerialization.data(withJSONObject: rec),
          var line = String(data: data, encoding: .utf8) else { return }
    line.append("\n")
    let dir = (kEventsLogPath as NSString).deletingLastPathComponent
    try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
    if let fh = FileHandle(forWritingAtPath: kEventsLogPath) {
        defer { try? fh.close() }
        try? fh.seekToEnd()
        try? fh.write(contentsOf: Data(line.utf8))
    } else {
        // First write: create.
        try? line.write(toFile: kEventsLogPath, atomically: true, encoding: .utf8)
    }
}

func bundleResourcesURL() -> URL { Bundle.main.resourceURL ?? URL(fileURLWithPath: ".") }
func bundleScriptURL(_ name: String) -> URL {
    // Contents/Resources/scripts/<name>.sh — kept here (not Helpers/MacOS) so
    // codesign treats them as bundle resources, not nested code components.
    bundleResourcesURL().appendingPathComponent("scripts").appendingPathComponent(name)
}

func currentRepo() -> String? {
    if let s = try? String(contentsOfFile: kActiveRepoPath, encoding: .utf8) {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if !t.isEmpty, FileManager.default.fileExists(atPath: t) { return t }
    }
    if let s = try? String(contentsOfFile: kReposListPath, encoding: .utf8) {
        for line in s.components(separatedBy: "\n") {
            let t = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if !t.isEmpty, !t.hasPrefix("#"), FileManager.default.fileExists(atPath: t) { return t }
        }
    }
    return nil
}

func setCurrentRepo(_ path: String) {
    let dir = (kActiveRepoPath as NSString).deletingLastPathComponent
    try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
    try? path.write(toFile: kActiveRepoPath, atomically: true, encoding: .utf8)
}

// Cheap Taskfile.yml parser: top-level task names match `^  [a-z][a-z0-9_-]*:`.
func parseTaskfile(_ repo: String) -> [String] {
    let path = repo + "/Taskfile.yml"
    guard let body = try? String(contentsOfFile: path, encoding: .utf8) else { return [] }
    var inTasks = false
    var out: [String] = []
    for line in body.components(separatedBy: "\n") {
        if line.hasPrefix("tasks:") { inTasks = true; continue }
        guard inTasks else { continue }
        // bare top-level key resets `tasks` block
        if !line.hasPrefix(" "), !line.hasPrefix("\t"), line.contains(":"), !line.trimmingCharacters(in: .whitespaces).isEmpty {
            inTasks = false; continue
        }
        // exactly two-space indent + identifier + colon
        if line.count > 3, line.hasPrefix("  "), !line.hasPrefix("   "), line.hasSuffix(":") || line.contains(":") {
            let trimmed = line.dropFirst(2)
            if let colon = trimmed.firstIndex(of: ":") {
                let name = String(trimmed[..<colon])
                let ok = name.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" }
                if ok, !name.isEmpty, name.first!.isLowercase || name.first!.isLetter {
                    out.append(name)
                }
            }
        }
    }
    return out
}

// MARK: - shell helpers

@discardableResult
func runDetached(_ argv: [String]) -> Process {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: argv[0])
    p.arguments = Array(argv.dropFirst())
    do { try p.run() } catch { NSLog("crabcc: spawn failed: \(error)") }
    return p
}

// Synchronously capture stdout from a short-running command. Returns "" on
// any failure — callers must tolerate that. Used only for process probes
// (pgrep / ps) where the call is bounded < 100 ms.
func captureStdout(_ argv: [String]) -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: argv[0])
    p.arguments = Array(argv.dropFirst())
    let out = Pipe()
    p.standardOutput = out
    p.standardError = Pipe()
    do {
        try p.run()
        p.waitUntilExit()
    } catch { return "" }
    let data = out.fileHandleForReading.readDataToEndOfFile()
    return String(data: data, encoding: .utf8) ?? ""
}

// Count processes whose argv matches the given pattern (pgrep -f -c).
// Self-pgrep filtered out: pgrep itself never matches because we pass -f
// against `crabcc <subcommand>` patterns that only the real binary uses.
func pgrepCount(_ pattern: String) -> Int {
    let s = captureStdout(["/usr/bin/pgrep", "-f", "-c", pattern])
        .trimmingCharacters(in: .whitespacesAndNewlines)
    return Int(s) ?? 0
}

// Count active agent runs. Prefer the singleton SQLite store at
// ~/.crabcc/_internal.db (status='running' rows). Fall back to walking
// ~/.crabcc/agents/<id>/lock — agent.rs creates that file at run start
// and finalizes (deletes) it on graceful exit, so its presence is the
// canonical "this run is in flight" signal.
func activeAgentRunCount() -> Int {
    if let n = activeAgentRunCountFromDb() { return n }
    let runsRoot = (NSString("~/.crabcc/agents") as NSString).expandingTildeInPath
    guard let entries = try? FileManager.default.contentsOfDirectory(atPath: runsRoot) else { return 0 }
    var count = 0
    for entry in entries {
        let lock = "\(runsRoot)/\(entry)/lock"
        if FileManager.default.fileExists(atPath: lock) { count += 1 }
    }
    return count
}

func activeAgentRunCountFromDb() -> Int? {
    let dbPath = (NSString("~/.crabcc/_internal.db") as NSString).expandingTildeInPath
    guard FileManager.default.fileExists(atPath: dbPath) else { return nil }
    let s = captureStdout(["/usr/bin/sqlite3", "-readonly", dbPath,
                           "SELECT count(*) FROM agent_runs WHERE status='running';"])
        .trimmingCharacters(in: .whitespacesAndNewlines)
    return Int(s)
}

struct KillEvent { let runId: String; let reason: String; let detail: String }

struct ScheduledTask {
    let label: String
    let cadence: String       // "every 20 min" / "at login" / "kept alive"
    let lastExitCode: Int?    // last reported exit (nil = never run / unknown)
    let pid: Int?             // current PID (nil = not running)
    let isRunning: Bool
}

// Walk ~/Library/LaunchAgents for our com.crabcc.* labels, then ask
// launchctl about each one. Pure parse — no daemon hits.
func scheduledTasks() -> [ScheduledTask] {
    let dir = NSString(string: "~/Library/LaunchAgents").expandingTildeInPath
    guard let entries = try? FileManager.default.contentsOfDirectory(atPath: dir) else { return [] }
    let labels = entries
        .filter { $0.hasPrefix("com.crabcc.") && $0.hasSuffix(".plist") }
        .map { String($0.dropLast(".plist".count)) }
        .sorted()

    let uid = getuid()
    return labels.map { label -> ScheduledTask in
        let body = captureStdout(["/bin/launchctl", "print", "gui/\(uid)/\(label)"])
        let pid: Int? = body
            .split(separator: "\n")
            .first(where: { $0.contains("pid =") })
            .flatMap { line in Int(String(line).split(separator: "=").last?.trimmingCharacters(in: .whitespaces) ?? "") }
        let lastExit: Int? = body
            .split(separator: "\n")
            .first(where: { $0.contains("last exit code =") })
            .flatMap { line in Int(String(line).split(separator: "=").last?.trimmingCharacters(in: .whitespaces) ?? "") }
        let plistPath = "\(dir)/\(label).plist"
        let plistBody = (try? String(contentsOfFile: plistPath, encoding: .utf8)) ?? ""
        let cadence: String
        if let interval = matchInteger(body: plistBody, key: "StartInterval") {
            cadence = humanizeInterval(interval)
        } else if plistBody.contains("<key>KeepAlive</key>") {
            cadence = "kept alive"
        } else if plistBody.contains("<key>RunAtLoad</key>") {
            cadence = "at login"
        } else {
            cadence = "manual"
        }
        return ScheduledTask(label: label, cadence: cadence,
                             lastExitCode: lastExit, pid: pid, isRunning: pid != nil)
    }
}

// Scrape an `<integer>N</integer>` value following a `<key>X</key>` line.
// Cheap regex avoids pulling in a full plist parser for one int.
func matchInteger(body: String, key: String) -> Int? {
    guard let keyRange = body.range(of: "<key>\(key)</key>") else { return nil }
    let tail = body[keyRange.upperBound...]
    guard let intStart = tail.range(of: "<integer>"),
          let intEnd   = tail.range(of: "</integer>", range: intStart.upperBound..<tail.endIndex)
    else { return nil }
    return Int(tail[intStart.upperBound..<intEnd.lowerBound])
}

func humanizeInterval(_ secs: Int) -> String {
    if secs < 60 { return "every \(secs)s" }
    if secs < 3600 { return "every \(secs / 60) min" }
    return "every \(secs / 3600) h"
}

func recentKillEvents(limit: Int) -> [KillEvent] {
    let dbPath = (NSString("~/.crabcc/_internal.db") as NSString).expandingTildeInPath
    guard FileManager.default.fileExists(atPath: dbPath) else { return [] }
    // Pipe-delimited output for cheap parsing — sqlite3 -separator '|' default.
    let raw = captureStdout(["/usr/bin/sqlite3", "-readonly", dbPath,
        "SELECT run_id || '|' || reason || '|' || COALESCE(detail,'') FROM agent_kill_events ORDER BY killed_at DESC LIMIT \(limit);"])
    return raw.split(separator: "\n").compactMap { line in
        let parts = line.split(separator: "|", maxSplits: 2, omittingEmptySubsequences: false)
        guard parts.count == 3 else { return nil }
        return KillEvent(runId: String(parts[0]), reason: String(parts[1]), detail: String(parts[2]))
    }
}

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
    runDetached(["/usr/bin/osascript", "-e", script])
}

// MARK: - app delegate

class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    var statusItem: NSStatusItem!

    func applicationDidFinishLaunching(_ note: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            if let img = NSImage(systemSymbolName: "shippingbox.fill", accessibilityDescription: "Crabcc") {
                img.isTemplate = true
                button.image = img
            } else {
                button.title = "C"
            }
        }
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
        rebuildMenu()
        let v = (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "dev"
        emitEvent("app_launched", ["version": v])
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        emitEvent("menu_opened")
        rebuildMenu()
    }

    func rebuildMenu() {
        guard let menu = statusItem.menu else { return }
        menu.removeAllItems()

        let v = (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "dev"
        let header = NSMenuItem(title: "Crabcc \(v)", action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)

        let repo = currentRepo()
        let repoItem = NSMenuItem(
            title: "Active repo: \(repo.map { ($0 as NSString).lastPathComponent } ?? "(none)")",
            action: #selector(pickRepo(_:)), keyEquivalent: "")
        repoItem.target = self
        menu.addItem(repoItem)

        menu.addItem(.separator())

        // Live process counts. Probes run synchronously (~10 ms total) on
        // every menu open via menuNeedsUpdate, so values are always fresh.
        let nIndex   = pgrepCount("crabcc index")
        let nWatch   = pgrepCount("crabcc watch")
        let nAgent   = pgrepCount("crabcc agent") + activeAgentRunCount()
        let nAgentd  = pgrepCount("crabcc-agentd")
        let statusItems: [(String, Int, String)] = [
            ("indexes",  nIndex,  "shippingbox.fill"),
            ("watches",  nWatch,  "eye.fill"),
            ("agents",   nAgent,  "person.fill"),
            ("agentd",   nAgentd, "gearshape.2.fill"),
        ]
        for (label, n, sym) in statusItems {
            let mi = NSMenuItem(
                title: "\(label): \(n)" + (n > 0 ? "" : "  (idle)"),
                action: nil, keyEquivalent: "")
            if let img = NSImage(systemSymbolName: sym, accessibilityDescription: label) {
                img.isTemplate = true
                mi.image = img
            }
            mi.isEnabled = false
            menu.addItem(mi)
        }

        menu.addItem(.separator())

        // Run Task submenu
        let runMenu = NSMenu()
        if let r = repo {
            let names = parseTaskfile(r)
            if names.isEmpty {
                let none = NSMenuItem(title: "(no tasks found in Taskfile.yml)", action: nil, keyEquivalent: "")
                none.isEnabled = false
                runMenu.addItem(none)
            } else {
                for n in names {
                    let mi = NSMenuItem(title: n, action: #selector(runTask(_:)), keyEquivalent: "")
                    mi.target = self
                    mi.representedObject = ["repo": r, "task": n]
                    runMenu.addItem(mi)
                }
            }
        } else {
            let none = NSMenuItem(title: "(pick a repo first)", action: nil, keyEquivalent: "")
            none.isEnabled = false
            runMenu.addItem(none)
        }
        let runItem = NSMenuItem(title: "Run Task", action: nil, keyEquivalent: "")
        runItem.submenu = runMenu
        menu.addItem(runItem)

        // Scheduled tasks (LaunchAgents) — what's wired to fire on a
        // cadence and the live state of each.
        let schedMenu = NSMenu()
        let tasks = scheduledTasks()
        if tasks.isEmpty {
            let none = NSMenuItem(title: "(no LaunchAgents installed)", action: nil, keyEquivalent: "")
            none.isEnabled = false
            schedMenu.addItem(none)
        } else {
            for t in tasks {
                let runState = t.isRunning ? "● running pid=\(t.pid ?? 0)" : "○ idle"
                let exitTail = t.lastExitCode.map { " · last_exit=\($0)" } ?? ""
                let mi = NSMenuItem(
                    title: "\(t.label.replacingOccurrences(of: "com.crabcc.", with: "")) · \(t.cadence) · \(runState)\(exitTail)",
                    action: #selector(kickstartTask(_:)), keyEquivalent: "")
                mi.target = self
                mi.representedObject = t.label
                if let img = NSImage(systemSymbolName: t.isRunning ? "circle.fill" : "circle.dotted",
                                     accessibilityDescription: nil) {
                    img.isTemplate = true
                    mi.image = img
                }
                schedMenu.addItem(mi)
            }
        }
        let schedItem = NSMenuItem(title: "Scheduled Tasks (\(tasks.count))", action: nil, keyEquivalent: "")
        schedItem.submenu = schedMenu
        menu.addItem(schedItem)

        // Recent kill events (from the singleton DB). Surfaces what the
        // 20-min agent-guard cleaned up so issues don't go silent.
        let killsMenu = NSMenu()
        let kills = recentKillEvents(limit: 10)
        if kills.isEmpty {
            let none = NSMenuItem(title: "(no kills recorded)", action: nil, keyEquivalent: "")
            none.isEnabled = false
            killsMenu.addItem(none)
        } else {
            for k in kills {
                let mi = NSMenuItem(
                    title: "\(k.runId.prefix(10))… · \(k.reason) · \(k.detail.prefix(50))",
                    action: #selector(openKillLog(_:)), keyEquivalent: "")
                mi.target = self
                mi.representedObject = k.runId
                killsMenu.addItem(mi)
            }
        }
        let killsItem = NSMenuItem(title: "Recent Kills (\(kills.count))", action: nil, keyEquivalent: "")
        killsItem.submenu = killsMenu
        menu.addItem(killsItem)

        menu.addItem(.separator())
        menu.addItem(item("Reindex Now",         #selector(reindexNow)))
        menu.addItem(item("Run Guard Now",       #selector(runGuardNow)))
        menu.addItem(item("Open Logs",           #selector(openLogs)))
        menu.addItem(item("Reinstall / Update…", #selector(reinstall)))
        menu.addItem(.separator())
        menu.addItem(item("About Crabcc…",       #selector(showAbout)))
        menu.addItem(item("Quit Crabcc",         #selector(NSApplication.terminate(_:)), key: "q"))
        // Escape hatch: NSApplication.terminate(_:) is queued behind any
        // running modal alert and won't fire if showAbout()'s runModal()
        // wedges (e.g. alert hidden behind a fullscreen app, focus lost).
        // exit(0) bypasses AppKit entirely.
        menu.addItem(item("Force Quit (skip modals)", #selector(forceQuit), key: "Q"))
    }

    func item(_ title: String, _ sel: Selector, key: String = "") -> NSMenuItem {
        let mi = NSMenuItem(title: title, action: sel, keyEquivalent: key)
        mi.target = self
        return mi
    }

    // MARK: - menu actions

    @objc func runTask(_ sender: NSMenuItem) {
        guard let info = sender.representedObject as? [String: String],
              let repo = info["repo"], let name = info["task"] else { return }
        emitEvent("task_run", ["repo": repo, "task": name])
        runInTerminal(repo: repo, command: "task \(name)")
    }

    @objc func reindexNow() {
        guard let r = currentRepo() else { return }
        emitEvent("reindex_invoked", ["repo": r])
        runInTerminal(repo: r, command: "crabcc index")
    }

    @objc func runGuardNow() {
        emitEvent("guard_invoked")
        let crabcc = NSString(string: "~/.cargo/bin/crabcc").expandingTildeInPath
        runDetached([crabcc, "agent-guard", "--json"])
    }

    @objc func kickstartTask(_ sender: NSMenuItem) {
        guard let label = sender.representedObject as? String else { return }
        emitEvent("launchagent_kickstart", ["label": label])
        let uid = String(getuid())
        runDetached(["/bin/launchctl", "kickstart", "-k", "gui/\(uid)/\(label)"])
    }

    @objc func openKillLog(_ sender: NSMenuItem) {
        guard let runId = sender.representedObject as? String else { return }
        emitEvent("kill_log_opened", ["run_id": runId])
        let path = NSString(string: "~/.crabcc/agents/\(runId)/.agent-\(runId)-kill-log").expandingTildeInPath
        if FileManager.default.fileExists(atPath: path) {
            runDetached(["/usr/bin/open", "-t", path])
        } else {
            let dir = NSString(string: "~/.crabcc/agents/\(runId)").expandingTildeInPath
            runDetached(["/usr/bin/open", dir])
        }
    }

    @objc func openLogs() {
        try? FileManager.default.createDirectory(atPath: kLogsDir, withIntermediateDirectories: true)
        runDetached(["/usr/bin/open", kLogsDir])
    }

    @objc func showAbout() {
        let v = (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "dev"
        let id = (Bundle.main.infoDictionary?["CFBundleIdentifier"] as? String) ?? ""
        let bundlePath = Bundle.main.bundlePath
        let cliVersion = captureStdout([NSString(string: "~/.cargo/bin/crabcc").expandingTildeInPath, "--version"])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let agentdLoaded = captureStdout(["/bin/launchctl", "print", "gui/\(getuid())/com.crabcc.agentd"])
            .contains("state = running")

        let alert = NSAlert()
        alert.messageText = "Crabcc \(v)"
        alert.informativeText = """
        Identifier: \(id)
        Bundle:     \(bundlePath)
        CLI:        \(cliVersion.isEmpty ? "(crabcc not on PATH)" : cliVersion)
        agentd:     \(agentdLoaded ? "running (LaunchAgent)" : "not loaded")
        Repos file: \(kReposListPath)
        Logs:       \(kLogsDir)
        """
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Open Repo Page")
        alert.addButton(withTitle: "Close")
        NSApp.activate(ignoringOtherApps: true)
        // Floating level keeps the alert above fullscreen apps and other
        // windows — without this it can land off-screen / behind a Space
        // and the user sees an app that "won't respond" because runModal()
        // is blocking the main thread on an invisible alert.
        alert.window.level = .floating
        let resp = alert.runModal()
        if resp == .alertFirstButtonReturn,
           let url = URL(string: "https://github.com/peterlodri-sec/crabcc") {
            NSWorkspace.shared.open(url)
        }
    }

    @objc func reinstall() {
        emitEvent("reinstall_invoked")
        let updater = bundleScriptURL("update.sh").path
        let installerLog = (NSString("~/Library/Logs/Crabcc/installer.log") as NSString).expandingTildeInPath
        let cmd = "bash \(updater.replacingOccurrences(of: "\"", with: "\\\""))"
        let script = """
        tell application "Terminal"
            activate
            do script "\(cmd) 2>&1 | tee \(installerLog)"
        end tell
        """
        runDetached(["/usr/bin/osascript", "-e", script])
    }

    @objc func forceQuit() {
        emitEvent("force_quit")
        // exit() bypasses NSApp.terminate, applicationShouldTerminate, and
        // any in-flight modal alert blocking the main thread. Last-resort
        // for when the regular Quit menu item is unresponsive.
        exit(0)
    }

    @objc func pickRepo(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.title = "Pick a crabcc repo"
        if panel.runModal() == .OK, let url = panel.url {
            setCurrentRepo(url.path)
            // Also append to repos.list (dedup)
            let existing = (try? String(contentsOfFile: kReposListPath, encoding: .utf8)) ?? ""
            if !existing.components(separatedBy: "\n").contains(url.path) {
                let dir = (kReposListPath as NSString).deletingLastPathComponent
                try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
                let updated = (existing.isEmpty ? "" : existing.trimmingCharacters(in: .whitespacesAndNewlines) + "\n") + url.path + "\n"
                try? updated.write(toFile: kReposListPath, atomically: true, encoding: .utf8)
            }
            rebuildMenu()
        }
    }
}

// MARK: - main

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = AppDelegate()
app.delegate = delegate
app.run()
