// telegram.swift — Telegram-bot section in the menubar (issue #156).
//
// Compiled at build time alongside menubar.swift / sticky.swift by
// scripts/build-dmg.sh:
//   swiftc -O -parse-as-library -o Crabcc *.swift
//
// Pure read helpers. No state, no AppDelegate dependencies. The @objc
// menu actions live in menubar.swift's AppDelegate (Cocoa requires the
// `target` be the menu's owning class). This file only provides the
// state-reading + presentation helpers the rebuildMenu() call needs.

import AppKit
import Foundation

// MARK: - state shape

struct TelegramBotState {
    let pid: Int?
    let lastExitCode: Int?
    let uptimeSeconds: Int?

    var isRunning: Bool { pid != nil }
}

// Returns nil when no com.crabcc.telegram-bot.plist exists in
// ~/Library/LaunchAgents — keeps the menu clean pre-install (per #156
// acceptance criterion: "Hidden when no plist exists at all").
func telegramBotState() -> TelegramBotState? {
    let plistPath = NSString(string: "~/Library/LaunchAgents/com.crabcc.telegram-bot.plist").expandingTildeInPath
    guard FileManager.default.fileExists(atPath: plistPath) else { return nil }

    let uid = getuid()
    let body = captureStdout(["/bin/launchctl", "print", "gui/\(uid)/com.crabcc.telegram-bot"])

    let pid: Int? = body
        .split(separator: "\n")
        .first(where: { $0.contains("pid =") })
        .flatMap { line in Int(String(line).split(separator: "=").last?.trimmingCharacters(in: .whitespaces) ?? "") }
    let lastExit: Int? = body
        .split(separator: "\n")
        .first(where: { $0.contains("last exit code =") })
        .flatMap { line in Int(String(line).split(separator: "=").last?.trimmingCharacters(in: .whitespaces) ?? "") }

    let uptime: Int? = pid.flatMap { p -> Int? in
        let raw = captureStdout(["/bin/ps", "-o", "etime=", "-p", String(p)])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return parseEtime(raw)
    }

    return TelegramBotState(pid: pid, lastExitCode: lastExit, uptimeSeconds: uptime)
}

// `ps -o etime=` returns "[[dd-]hh:]mm:ss". Cheap parser; bounded loop;
// returns nil on any malformed input rather than silently zeroing.
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
    if secs < 60        { return "\(secs)s up" }
    if secs < 3600      { return "\(secs / 60)m up" }
    if secs < 86400     { return "\(secs / 3600)h up" }
    return "\(secs / 86400)d up"
}

// MARK: - presentation

// "● Telegram Bot · running pid=23123 · 2h up" /
// "◐ Telegram Bot · idle · last_exit=137" /
// "○ Telegram Bot · idle"
//
// The leading glyph is a fallback for environments that strip the SF
// Symbol image (e.g. older macOS or screen readers); the colored image
// in `telegramHealthImage` is the primary visual cue.
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

// Colored SF Symbol — green / yellow / gray to match the prefix glyph.
// `paletteColors` requires macOS 12+; build target is 13.0 so always safe.
// Returns nil only if the SF Symbol itself is missing on the host system.
func telegramHealthImage(_ bot: TelegramBotState) -> NSImage? {
    let symName: String
    let color: NSColor
    if bot.isRunning {
        symName = "circle.fill"
        color = .systemGreen
    } else if let exit = bot.lastExitCode, exit != 0 {
        symName = "exclamationmark.circle.fill"
        color = .systemYellow
    } else {
        symName = "circle.dotted"
        color = .systemGray
    }
    guard let base = NSImage(systemSymbolName: symName, accessibilityDescription: "Telegram Bot status") else { return nil }
    let cfg = NSImage.SymbolConfiguration(paletteColors: [color])
    let colored = base.withSymbolConfiguration(cfg) ?? base
    colored.isTemplate = false
    return colored
}
