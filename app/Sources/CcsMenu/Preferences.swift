// What the app remembers between launches: which daemons to run, and how.

import Foundation

final class Preferences: ObservableObject {
    private let defaults: UserDefaults

    @Published var gatewayOn: Bool { didSet { defaults.set(gatewayOn, forKey: "gateway.on") } }
    @Published var gatewayPort: Int { didSet { defaults.set(gatewayPort, forKey: "gateway.port") } }
    @Published var rotationOn: Bool { didSet { defaults.set(rotationOn, forKey: "rotation.on") } }
    /// Slugs, in the order they were ticked.
    @Published var pool: [String] { didSet { defaults.set(pool, forKey: "rotation.pool") } }
    @Published var notificationsOn: Bool { didSet { defaults.set(notificationsOn, forKey: "notifications.on") } }
    @Published var refreshSeconds: Int { didSet { defaults.set(refreshSeconds, forKey: "refresh.seconds") } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        gatewayOn = defaults.bool(forKey: "gateway.on")
        gatewayPort = defaults.object(forKey: "gateway.port") as? Int ?? 4141
        rotationOn = defaults.bool(forKey: "rotation.on")
        pool = defaults.stringArray(forKey: "rotation.pool") ?? []
        notificationsOn = defaults.object(forKey: "notifications.on") as? Bool ?? true
        refreshSeconds = defaults.object(forKey: "refresh.seconds") as? Int ?? 300
    }

    func toggle(_ slug: String, inPool on: Bool) {
        pool.removeAll { $0 == slug }
        if on { pool.append(slug) }
    }
}
