// The app's one source of state: the last reading, the daemons, and the
// preferences that decide how the daemons run. Everything the popover shows
// and everything it can do goes through here.
//
// The readings are what `ccs watch` last wrote down, read back cheaply and
// often; the watcher is the one thing that polls, at its own interval, so
// the app never spends the limits it displays.

import Foundation
import UserNotifications

@MainActor
final class Store: ObservableObject {
    @Published private(set) var accounts: [Account] = []
    @Published private(set) var updatedAt: Date?
    @Published private(set) var error: String?
    @Published private(set) var refreshing = false
    /// What the last switch said, shown until the next reading.
    @Published private(set) var said: String?

    let preferences: Preferences
    let ccs: Ccs?
    let gateway = Daemon(name: "gateway")
    let watcher = Daemon(name: "watcher")

    private var timer: Timer?
    private var notificationsAllowed = false

    init(preferences: Preferences = Preferences(), ccs: Ccs? = Ccs.locate()) {
        self.preferences = preferences
        self.ccs = ccs
        if ccs == nil {
            error = "ccs is not installed; `make install` in the ccs checkout puts it in ~/.cargo/bin"
        }
        askForNotifications()
        schedule()
        apply()
        Task { await refresh() }
    }

    var active: Account? { accounts.first { $0.active } }

    /// The label for the menu bar: the active account's session percentage.
    var label: String {
        guard let session = active?.session else { return "–" }
        return "\(Int(session.percent.rounded()))%"
    }

    var labelHealth: Health { active?.session?.health ?? .ok }

    // MARK: readings

    func refresh() async {
        guard let ccs, !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        do {
            accounts = try await ccs.list()
            updatedAt = Date()
            error = nil
            said = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    func switchTo(_ slug: String, force: Bool) async {
        guard let ccs else { return }
        do {
            said = try await ccs.use(slug, force: force).split(separator: "\n").first.map(String.init)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        await refresh()
    }

    /// When the newest reading on show was taken.
    var polledAt: Date? { accounts.compactMap(\.polledAt).max() }

    private func schedule() {
        timer?.invalidate()
        // Reading the cache is cheap, so this can be far more often than a poll.
        let every = TimeInterval(max(min(preferences.refreshSeconds, 60), 15))
        timer = Timer.scheduledTimer(withTimeInterval: every, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in await self?.refresh() }
        }
    }

    // MARK: daemons

    /// Bring the daemons in line with the preferences. Called after any change
    /// to them; a child whose arguments changed is restarted.
    func apply() {
        guard let ccs else { return }
        let pool = preferences.pool

        let serve = serveArguments(port: preferences.gatewayPort, pool: pool)
        if preferences.gatewayOn {
            if !gateway.running || gatewayArguments != serve {
                gateway.start(binary: ccs.binary, arguments: serve) { _ in }
                gatewayArguments = serve
            }
        } else {
            gateway.stop()
            gatewayArguments = nil
        }

        // The watcher always runs: it is the poller the readings come from.
        // Rotation and notifications only change what is done with a poll.
        let watch = watchArguments(pool: preferences.rotationOn ? pool : [])
        if !watcher.running || watcherArguments != watch {
            watcher.start(binary: ccs.binary, arguments: watch) { [weak self] line in
                self?.heard(line)
            }
            watcherArguments = watch
        }
    }

    private var gatewayArguments: [String]?
    private var watcherArguments: [String]?

    func shutdown() {
        gateway.stop()
        watcher.stop()
    }

    private func heard(_ line: String) {
        // Every line the watcher prints follows a poll, so the cache is fresh.
        Task { await refresh() }
        guard let notice = parseNotice(line) else { return }
        if preferences.notificationsOn { notify(notice) }
    }

    // MARK: notifications

    private func askForNotifications() {
        // Only a bundle can post notifications; the bare executable cannot.
        guard Bundle.main.bundleIdentifier != nil else { return }
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { [weak self] granted, _ in
            Task { @MainActor [weak self] in self?.notificationsAllowed = granted }
        }
    }

    private func notify(_ notice: Notice) {
        guard notificationsAllowed else { return }
        let content = UNMutableNotificationContent()
        content.title = notice.title
        content.body = notice.body
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}
