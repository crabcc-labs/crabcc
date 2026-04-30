// Repo.swift — active-repo + repos.list state at ~/.crabcc/agent/.
//
// Direct port of `currentRepo`, `setCurrentRepo`, and the
// repos.list dedup logic from the legacy menubar.swift.

import Dependencies
import Foundation

struct RepoClient {
    var current: @Sendable () -> String?
    var setCurrent: @Sendable (_ path: String) -> Void
    var parseTaskfile: @Sendable (_ repo: String) -> [String]
}

extension RepoClient: DependencyKey {
    static let liveValue: RepoClient = {
        let activeRepoPath = NSString(string: "~/.crabcc/agent/active-repo")
            .expandingTildeInPath
        let reposListPath = NSString(string: "~/.crabcc/agent/repos.list")
            .expandingTildeInPath

        return RepoClient(
            current: {
                if let s = try? String(contentsOfFile: activeRepoPath, encoding: .utf8) {
                    let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
                    if !t.isEmpty, FileManager.default.fileExists(atPath: t) {
                        return t
                    }
                }
                if let s = try? String(contentsOfFile: reposListPath, encoding: .utf8) {
                    for line in s.components(separatedBy: "\n") {
                        let t = line.trimmingCharacters(in: .whitespacesAndNewlines)
                        if !t.isEmpty, !t.hasPrefix("#"),
                           FileManager.default.fileExists(atPath: t) {
                            return t
                        }
                    }
                }
                return nil
            },
            setCurrent: { path in
                let dir = (activeRepoPath as NSString).deletingLastPathComponent
                try? FileManager.default.createDirectory(
                    atPath: dir, withIntermediateDirectories: true
                )
                try? path.write(toFile: activeRepoPath, atomically: true, encoding: .utf8)

                // Append to repos.list (dedup).
                let existing =
                    (try? String(contentsOfFile: reposListPath, encoding: .utf8)) ?? ""
                if !existing.components(separatedBy: "\n").contains(path) {
                    let updated = (existing.isEmpty
                        ? ""
                        : existing.trimmingCharacters(in: .whitespacesAndNewlines) + "\n"
                    ) + path + "\n"
                    try? updated.write(
                        toFile: reposListPath, atomically: true, encoding: .utf8
                    )
                }
            },
            parseTaskfile: { repo in
                parseTaskfile(repo)
            }
        )
    }()

    static let testValue = RepoClient(
        current: { nil },
        setCurrent: { _ in },
        parseTaskfile: { _ in [] }
    )
}

extension DependencyValues {
    var repo: RepoClient {
        get { self[RepoClient.self] }
        set { self[RepoClient.self] = newValue }
    }
}

// MARK: - Taskfile.yml parser
//
// Cheap regex-free parser: top-level task names match exactly two-space
// indent + identifier + colon. Direct port of the legacy menubar.swift
// `parseTaskfile`.

func parseTaskfile(_ repo: String) -> [String] {
    let path = repo + "/Taskfile.yml"
    guard let body = try? String(contentsOfFile: path, encoding: .utf8) else { return [] }
    var inTasks = false
    var out: [String] = []
    for line in body.components(separatedBy: "\n") {
        if line.hasPrefix("tasks:") { inTasks = true; continue }
        guard inTasks else { continue }
        if !line.hasPrefix(" "), !line.hasPrefix("\t"), line.contains(":"),
           !line.trimmingCharacters(in: .whitespaces).isEmpty {
            inTasks = false
            continue
        }
        if line.count > 3, line.hasPrefix("  "), !line.hasPrefix("   "),
           line.hasSuffix(":") || line.contains(":") {
            let trimmed = line.dropFirst(2)
            if let colon = trimmed.firstIndex(of: ":") {
                let name = String(trimmed[..<colon])
                let ok = name.allSatisfy {
                    $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_"
                }
                if ok, !name.isEmpty {
                    out.append(name)
                }
            }
        }
    }
    return out
}
