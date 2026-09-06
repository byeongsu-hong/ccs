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
    private var pipe: Pipe?
    private var buffer = Data()
    private let keep = 6

    /// The child's pid while it runs, so a later launch can find it if this
    /// one never got to stop it.
    var pid: Int32? { process?.processIdentifier }

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
            // At end of file the handler is called again and again until it
            // is taken down; leaving it up spins a core for nothing.
            if chunk.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            Task { @MainActor [weak self] in
                self?.consume(chunk, onLine: onLine)
            }
        }
        process.terminationHandler = { [weak self] finished in
            Task { @MainActor [weak self] in
                guard let self, self.process === finished else { return }
                self.running = false
                self.process = nil
                self.pipe = nil
                // Merged stderr: a last line explains a failure; success has
                // nothing to explain and the last line would be a log line.
                let status = finished.terminationStatus
                self.exitedWith = status == 0
                    ? "exited"
                    : self.lines.last ?? "exited with status \(status)"
            }
        }
        do {
            try process.run()
            self.process = process
            self.pipe = pipe
            running = true
            exitedWith = nil
            lines = []
            buffer = Data()
        } catch {
            exitedWith = error.localizedDescription
            running = false
        }
    }

    func stop() {
        guard let process else { return }
        self.process = nil
        pipe?.fileHandleForReading.readabilityHandler = nil
        pipe = nil
        buffer = Data()
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
