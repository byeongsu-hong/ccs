// Finding the `ccs` binary and running it.
//
// The app is a shell over the CLI rather than a second implementation of it:
// every reading and every switch is the binary's doing, so the two never
// disagree, and a fix to token refresh or the keychain reaches the menu bar
// without a change here.

import Foundation

/// The places `ccs` is installed to, in the order to try them. A desktop app
/// does not inherit a shell's `PATH`, so nothing here searches it; the
/// environment override is for a build somewhere else entirely.
func ccsCandidates(
    environment: [String: String] = ProcessInfo.processInfo.environment,
    home: URL = FileManager.default.homeDirectoryForCurrentUser
) -> [URL] {
    var found: [URL] = []
    if let override = environment["CCS_BINARY"], !override.isEmpty {
        found.append(URL(fileURLWithPath: override))
    }
    found.append(home.appendingPathComponent(".cargo/bin/ccs"))
    found.append(URL(fileURLWithPath: "/usr/local/bin/ccs"))
    found.append(URL(fileURLWithPath: "/opt/homebrew/bin/ccs"))
    return found
}

/// What the binary said when it refused, without its own `ccs: ` prefix.
struct CcsError: LocalizedError, Equatable {
    var message: String
    var errorDescription: String? { message }
}

struct Ccs {
    let binary: URL

    /// The first installed binary, or nothing.
    static func locate() -> Ccs? {
        ccsCandidates().first { FileManager.default.isExecutableFile(atPath: $0.path) }.map(Ccs.init)
    }

    /// What the last poll wrote down, without polling. Asking the network
    /// here would spend the limits being shown, on every repaint; keeping
    /// the readings current is the watcher's job, and it runs for as long
    /// as the app does.
    func list() async throws -> [Account] {
        let out = try await run(["ls", "--cached", "--json"])
        return try decodeAccounts(Data(out.utf8))
    }

    /// Switch to `slug`. What the CLI printed comes back for the popover to show.
    func use(_ slug: String, force: Bool) async throws -> String {
        try await run(["use", slug] + (force ? ["--force"] : []))
    }

    /// Run the binary to completion and hand back its stdout. A non-zero exit
    /// is an error carrying the message the binary printed.
    func run(_ arguments: [String]) async throws -> String {
        let binary = self.binary
        return try await Task.detached(priority: .userInitiated) {
            let process = Process()
            process.executableURL = binary
            process.arguments = arguments
            // Never a terminal: the picker must not open, and colour is noise.
            process.environment = ProcessInfo.processInfo.environment.merging(["NO_COLOR": "1"]) { _, new in new }
            let stdout = Pipe()
            let stderr = Pipe()
            process.standardOutput = stdout
            process.standardError = stderr
            process.standardInput = FileHandle.nullDevice
            try process.run()
            let out = stdout.fileHandleForReading.readDataToEndOfFile()
            let err = stderr.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else {
                let said = String(decoding: err, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
                let line = said.split(separator: "\n").last.map(String.init) ?? "exit status \(process.terminationStatus)"
                throw CcsError(message: line.hasPrefix("ccs: ") ? String(line.dropFirst(5)) : line)
            }
            return String(decoding: out, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        }.value
    }
}
