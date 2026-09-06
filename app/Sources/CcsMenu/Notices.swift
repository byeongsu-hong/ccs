// What the daemons are told to do, and what the watcher says back.
//
// `ccs watch` prints one line per notice: a clock stamp, the kind, a colon,
// the text — and, when sessions were told, how many. The app reads those
// lines off the child's stdout and turns the ones it recognises into
// notifications. Anything else it prints is kept as a log line and no more.

import Foundation

enum Notice: Equatable {
    case sessionHigh(String)
    case sessionReset(String)
    case weeklyReset(String)
    case rotated(String)

    var title: String {
        switch self {
        case .sessionHigh: return "Session running high"
        case .sessionReset: return "Session reset"
        case .weeklyReset: return "Weekly reset"
        case .rotated: return "Rotated"
        }
    }

    var body: String {
        switch self {
        case let .sessionHigh(text), let .sessionReset(text), let .weeklyReset(text), let .rotated(text):
            return text
        }
    }
}

/// The notice on a watcher line, if it is one.
func parseNotice(_ line: String) -> Notice? {
    // "HH:MM:SS kind: text"
    let parts = line.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
    guard parts.count == 2 else { return nil }
    let rest = parts[1]
    guard let colon = rest.firstIndex(of: ":") else { return nil }
    let kind = rest[..<colon]
    var text = rest[rest.index(after: colon)...].trimmingCharacters(in: .whitespaces)
    // "; N sessions told" is the CLI's own record of delivery, not the news.
    if let heard = text.range(of: "; ", options: .backwards), text[heard.upperBound...].hasSuffix("told") {
        text = String(text[..<heard.lowerBound])
    }
    switch kind {
    case "session-high": return .sessionHigh(text)
    case "session-reset": return .sessionReset(text)
    case "weekly-reset": return .weeklyReset(text)
    case "rotate": return .rotated(text)
    default: return nil
    }
}

/// `ccs serve`'s arguments for a port, falling over to `pool` on a limit
/// when there is one.
func serveArguments(port: Int, pool: [String]) -> [String] {
    ["serve", "--port", String(port)] + rotate(pool)
}

/// `ccs watch`'s arguments, rotating between `pool` when there is one.
func watchArguments(pool: [String]) -> [String] {
    ["watch"] + rotate(pool)
}

private func rotate(_ pool: [String]) -> [String] {
    pool.isEmpty ? [] : ["--rotate", pool.joined(separator: ",")]
}
