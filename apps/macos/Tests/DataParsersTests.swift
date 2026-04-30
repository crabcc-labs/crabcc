// DataParsersTests.swift — data-check tests for the parser helpers.
//
// Per directive: data-check tests only, skip TCA TestStore unit tests
// for Phase 1. These cover the regex-free parsers that translate
// shell tool output into typed Swift values — the most fragile layer
// because shell output formats can shift across macOS versions.

import XCTest

@testable import Crabcc

final class DataParsersTests: XCTestCase {
    // MARK: - parseEtime

    func testParseEtimeSeconds() {
        XCTAssertEqual(parseEtime("42"), 42)
    }

    func testParseEtimeMinutesSeconds() {
        XCTAssertEqual(parseEtime("12:34"), 12 * 60 + 34)
    }

    func testParseEtimeHoursMinutesSeconds() {
        XCTAssertEqual(parseEtime("01:23:45"), 1 * 3600 + 23 * 60 + 45)
    }

    func testParseEtimeDaysHoursMinutesSeconds() {
        XCTAssertEqual(parseEtime("3-04:05:06"), 3 * 86400 + 4 * 3600 + 5 * 60 + 6)
    }

    func testParseEtimeMalformedReturnsNil() {
        XCTAssertNil(parseEtime(""))
        XCTAssertNil(parseEtime("not a duration"))
        XCTAssertNil(parseEtime("1:2:3:4")) // four colons → unsupported
    }

    // MARK: - humanizeUptime

    func testHumanizeUptimeSeconds() {
        XCTAssertEqual(humanizeUptime(42), "42s up")
    }

    func testHumanizeUptimeMinutes() {
        XCTAssertEqual(humanizeUptime(120), "2m up")
    }

    func testHumanizeUptimeHours() {
        XCTAssertEqual(humanizeUptime(3 * 3600), "3h up")
    }

    func testHumanizeUptimeDays() {
        XCTAssertEqual(humanizeUptime(2 * 86400), "2d up")
    }

    // MARK: - humanizeInterval

    func testHumanizeIntervalSeconds() {
        XCTAssertEqual(humanizeInterval(30), "every 30s")
    }

    func testHumanizeIntervalMinutes() {
        XCTAssertEqual(humanizeInterval(20 * 60), "every 20 min")
    }

    func testHumanizeIntervalHours() {
        XCTAssertEqual(humanizeInterval(2 * 3600), "every 2 h")
    }

    // MARK: - matchInteger

    func testMatchIntegerExtractsValue() {
        let plist = """
        <plist>
        <dict>
            <key>StartInterval</key>
            <integer>1200</integer>
        </dict>
        </plist>
        """
        XCTAssertEqual(matchInteger(body: plist, key: "StartInterval"), 1200)
    }

    func testMatchIntegerMissingKeyReturnsNil() {
        let plist = "<plist><dict></dict></plist>"
        XCTAssertNil(matchInteger(body: plist, key: "StartInterval"))
    }

    // MARK: - humanizeCadence (full plist body parse)

    func testHumanizeCadenceStartInterval() {
        let plist = """
        <plist><dict>
            <key>StartInterval</key>
            <integer>600</integer>
        </dict></plist>
        """
        XCTAssertEqual(humanizeCadence(plist: plist), "every 10 min")
    }

    func testHumanizeCadenceKeepAlive() {
        let plist = "<plist><dict><key>KeepAlive</key><true/></dict></plist>"
        XCTAssertEqual(humanizeCadence(plist: plist), "kept alive")
    }

    func testHumanizeCadenceRunAtLoad() {
        let plist = "<plist><dict><key>RunAtLoad</key><true/></dict></plist>"
        XCTAssertEqual(humanizeCadence(plist: plist), "at login")
    }

    func testHumanizeCadenceManualFallback() {
        XCTAssertEqual(humanizeCadence(plist: "<plist></plist>"), "manual")
    }

    // MARK: - parseTaskfile

    func testParseTaskfileEmpty() {
        let dir = NSTemporaryDirectory() + "crabcc-tasktest-empty-\(UUID().uuidString)"
        try! FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try! "version: '3'\n".write(
            toFile: "\(dir)/Taskfile.yml", atomically: true, encoding: .utf8
        )
        XCTAssertEqual(parseTaskfile(dir), [])
    }

    func testParseTaskfileExtractsTopLevelTasks() {
        let dir = NSTemporaryDirectory() + "crabcc-tasktest-\(UUID().uuidString)"
        try! FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let body = """
        version: '3'

        tasks:
          build:
            desc: build it
            cmds:
              - echo build
          test:
            desc: test it
          some-task_2:
            cmds:
              - echo
        """
        try! body.write(toFile: "\(dir)/Taskfile.yml", atomically: true, encoding: .utf8)
        XCTAssertEqual(parseTaskfile(dir), ["build", "test", "some-task_2"])
    }

    // MARK: - telegram header label

    func testTelegramHeaderRunning() {
        let bot = TelegramBotState(pid: 123, lastExitCode: 0, uptimeSeconds: 3600)
        let title = telegramHeaderTitle(bot)
        XCTAssertTrue(title.hasPrefix("● Telegram Bot · running pid=123"))
        XCTAssertTrue(title.contains("1h up"))
    }

    func testTelegramHeaderIdleNoCrash() {
        let bot = TelegramBotState(pid: nil, lastExitCode: 137, uptimeSeconds: nil)
        let title = telegramHeaderTitle(bot)
        XCTAssertTrue(title.hasPrefix("◐ Telegram Bot · idle"))
        XCTAssertTrue(title.contains("last_exit=137"))
    }

    func testTelegramHeaderIdleClean() {
        let bot = TelegramBotState(pid: nil, lastExitCode: nil, uptimeSeconds: nil)
        XCTAssertEqual(telegramHeaderTitle(bot), "○ Telegram Bot · idle")
    }
}
