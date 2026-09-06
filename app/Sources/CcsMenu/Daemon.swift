// A child `ccs` process the app keeps running for as long as it runs itself:
// the gateway, or the watcher. Its output is read a line at a time; the
// last few are kept for the popover, and every one is offered to a handler
// so the watcher's notices can become notifications.

import Foundation

@MainActor
final class Daemon: ObservableObject {
    let name: String
    @Published private(set) var running = false
    @Published private(set) var lines: [String] = []
    /// What it last said before exiting on its own, so a port already in
    /// use or a pool account that is not there is seen rather than retried.
    @Published private(set) var exitedWith: String?

    private var process: Process?
    private var buffer = Data()
    private let keep = 6

    init(name: String) {
        self.name = name
    }

    /// Start `binary` with `arguments`; a running child is stopped first.
    func start(binary: URL, arguments: [String], onLine: @escaping (String) -> Void) {
        stop()
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        process.environment = ProcessInfo.processInfo.environment.merging(["NO_COLOR": "1"]) { _, new in new }
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        process.standardInput = FileHandle.nullDevice

        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let chunk = handle.availableData
            Task { @MainActor [weak self] in
                self?.consume(chunk, onLine: onLine)
            }
        }
        process.terminationHandler = { [weak self] finished in
            Task { @MainActor [weak self] in
                guard let self, self.process === finished else { return }
                self.running = false
                self.process = nil
                self.exitedWith = self.lines.last ?? "exited with status \(finished.terminationStatus)"
            }
        }
        do {
            try process.run()
            self.process = process
            running = true
            exitedWith = nil
            lines = []
        } catch {
            exitedWith = error.localizedDescription
            running = false
        }
    }

    func stop() {
        guard let process else { return }
        self.process = nil
        process.terminationHandler = nil
        process.terminate()
        running = false
    }

    private func consume(_ chunk: Data, onLine: (String) -> Void) {
        buffer.append(chunk)
        while let newline = buffer.firstIndex(of: UInt8(ascii: "\n")) {
            let line = String(decoding: buffer[..<newline], as: UTF8.self)
            buffer.removeSubrange(...newline)
            guard !line.isEmpty else { continue }
            lines.append(line)
            if lines.count > keep { lines.removeFirst(lines.count - keep) }
            onLine(line)
        }
    }
}
