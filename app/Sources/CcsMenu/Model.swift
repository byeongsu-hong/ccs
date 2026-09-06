// What `ccs ls --json` reports, and the little the app derives from it.
//
// The shape mirrors the CLI's own: an account is a slug, an email, a plan and
// whatever limits the usage endpoint chose to report. Columns and health are
// worked out the same way the CLI works them out, so the menu bar and the
// terminal never disagree.

import Foundation

/// Percentage at or above which a limit is worth warning about, and at which
/// it has nothing left. The CLI's numbers.
private let warnPercent = 80.0
private let spentPercent = 100.0

enum Health: Equatable {
    case ok, warn, spent
}

struct Limit: Decodable, Equatable {
    var kind: String
    var percent: Double
    var severity: String?
    var resetsAt: Date?
    var scope: Scope?

    struct Scope: Decodable, Equatable {
        var model: Model?

        struct Model: Decodable, Equatable {
            var displayName: String?

            enum CodingKeys: String, CodingKey { case displayName = "display_name" }
        }
    }

    enum CodingKeys: String, CodingKey {
        case kind, percent, severity, scope
        case resetsAt = "resets_at"
    }

    init(kind: String, percent: Double, severity: String?, resetsAt: Date?, scope: Scope?) {
        self.kind = kind
        self.percent = percent
        self.severity = severity
        self.resetsAt = resetsAt
        self.scope = scope
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decode(String.self, forKey: .kind)
        percent = try container.decodeIfPresent(Double.self, forKey: .percent) ?? 0
        severity = try container.decodeIfPresent(String.self, forKey: .severity)
        resetsAt = try container.decodeIfPresent(String.self, forKey: .resetsAt).flatMap(parseInstant)
        scope = try container.decodeIfPresent(Scope.self, forKey: .scope)
    }

    /// The column this limit belongs under: the model's name when it is
    /// scoped to one, short names for the two unscoped families, and a
    /// readable form of anything newer.
    var column: String {
        if let name = scope?.model?.displayName { return name }
        switch kind {
        case "session": return "session"
        case "weekly_all": return "weekly"
        default: return kind.replacingOccurrences(of: "_", with: " ")
        }
    }

    var spent: Bool { percent >= spentPercent }

    /// The API's own severity is authoritative below the cap; the percentage
    /// stands in where it is absent.
    var health: Health {
        if spent { return .spent }
        switch severity {
        case "critical", "warning": return .warn
        case "normal": return .ok
        default: return percent >= warnPercent ? .warn : .ok
        }
    }
}

struct Account: Decodable, Identifiable, Equatable {
    var slug: String
    var email: String
    var plan: String
    var active: Bool
    var limits: [Limit]
    /// When the limits were read, for a listing that did not read them now.
    var polledAt: Date?

    enum CodingKeys: String, CodingKey {
        case slug, email, plan, active, limits
        case polledAt = "polled_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        slug = try container.decode(String.self, forKey: .slug)
        email = try container.decode(String.self, forKey: .email)
        plan = try container.decode(String.self, forKey: .plan)
        active = try container.decode(Bool.self, forKey: .active)
        limits = try container.decodeIfPresent([Limit].self, forKey: .limits) ?? []
        polledAt = try container.decodeIfPresent(String.self, forKey: .polledAt).flatMap(parseInstant)
    }

    var id: String { slug }
    var session: Limit? { limits.first { $0.kind == "session" } }
    var spent: Bool { limits.contains { $0.spent } }
}

func decodeAccounts(_ data: Data) throws -> [Account] {
    try JSONDecoder().decode([Account].self, from: data)
}

/// The API stamps reset times with six fractional digits, which Foundation's
/// ISO 8601 parser will not take; the fraction says nothing a countdown needs.
private func parseInstant(_ text: String) -> Date? {
    var trimmed = text
    if let dot = trimmed.firstIndex(of: "."),
       let end = trimmed[dot...].firstIndex(where: { $0 == "+" || $0 == "-" || $0 == "Z" }) {
        trimmed.removeSubrange(dot..<end)
    }
    return ISO8601DateFormatter().date(from: trimmed)
}

/// How long until `date`, the way the CLI prints it: the two largest units
/// that are not zero, and `now` once it has passed.
func until(_ date: Date, now: Date = Date()) -> String {
    let seconds = Int(date.timeIntervalSince(now))
    if seconds <= 0 { return "now" }
    let days = seconds / 86400
    let hours = seconds % 86400 / 3600
    let minutes = seconds % 3600 / 60
    if days > 0 { return "\(days)d\(hours)h" }
    if hours > 0 { return "\(hours)h\(minutes)m" }
    return "\(minutes)m"
}
