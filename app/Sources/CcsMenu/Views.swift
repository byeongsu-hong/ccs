// The popover: what every account has left, a click to switch, and the
// switches for the two daemons.

import ServiceManagement
import SwiftUI

struct PopoverView: View {
    @EnvironmentObject var store: Store
    @EnvironmentObject var preferences: Preferences
    @State private var confirming: Account?
    @State private var loginError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            accounts
            Divider()
            GatewaySection()
            Divider()
            RotationSection()
            Divider()
            footer
        }
        .padding(12)
        .frame(width: 360)
        .confirmationDialog(
            "\(confirming?.email ?? "") has nothing left on one of its limits. Switch anyway?",
            isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
            titleVisibility: .visible
        ) {
            Button("Switch anyway", role: .destructive) {
                if let target = confirming { Task { await store.switchTo(target.slug, force: true) } }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task { await store.refresh() }
    }

    private var accounts: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Accounts").font(.headline)
                Spacer()
                if store.refreshing {
                    ProgressView().controlSize(.small)
                } else {
                    Button { Task { await store.refresh() } } label: { Image(systemName: "arrow.clockwise") }
                        .buttonStyle(.borderless)
                        .help("Refresh")
                }
            }
            if store.accounts.isEmpty, store.error == nil {
                Text("Reading…").foregroundStyle(.secondary).font(.caption)
            }
            ForEach(store.accounts) { account in
                AccountRow(account: account)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        guard !account.active else { return }
                        if account.spent {
                            confirming = account
                        } else {
                            Task { await store.switchTo(account.slug, force: false) }
                        }
                    }
            }
            if let said = store.said {
                Text(said).font(.caption).foregroundStyle(.secondary)
            }
            if let error = store.error {
                Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled)
            }
        }
    }

    private var footer: some View {
        HStack {
            Toggle("Launch at login", isOn: launchAtLogin).toggleStyle(.checkbox).font(.caption)
                .help(loginError ?? "Registers the app in /Applications with Launch Services")
            if let loginError {
                Text(loginError).font(.caption2).foregroundStyle(.red).lineLimit(1)
            }
            Spacer()
            if let polledAt = store.polledAt {
                Text("polled \(polledAt, style: .relative) ago").font(.caption).foregroundStyle(.secondary)
                    .help("When the watcher last read the limits; the app itself never polls")
            }
            Button("Quit") { NSApplication.shared.terminate(nil) }.font(.caption)
        }
    }

    private var launchAtLogin: Binding<Bool> {
        Binding(
            get: { SMAppService.mainApp.status == .enabled },
            set: { on in
                do {
                    if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
                    loginError = nil
                } catch {
                    // Registering needs a bundle in a fixed place; said, not swallowed.
                    loginError = error.localizedDescription
                }
            })
    }
}

struct AccountRow: View {
    let account: Account

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Image(systemName: account.active ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(account.active ? Color.accentColor : Color.secondary)
                Text(account.email).fontWeight(account.active ? .semibold : .regular)
                Text(account.plan).font(.caption).foregroundStyle(.secondary)
                Spacer()
            }
            HStack(spacing: 8) {
                ForEach(account.limits) { limit in
                    BarView(limit: limit)
                }
            }
            .padding(.leading, 22)
        }
        .padding(.vertical, 3)
        .opacity(account.spent && !account.active ? 0.6 : 1)
    }
}

struct BarView: View {
    let limit: Limit

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                Text(limit.column).font(.caption2).foregroundStyle(.secondary)
                Text("\(Int(limit.percent.rounded()))%").font(.caption2).monospacedDigit()
                if let at = limit.resetsAt {
                    Text(until(at)).font(.caption2).foregroundStyle(.secondary).monospacedDigit()
                }
            }
            ProgressView(value: min(limit.percent, 100), total: 100)
                .tint(limit.health.color)
                .frame(width: 96)
        }
    }
}

extension Health {
    var color: Color {
        switch self {
        case .ok: return .green
        case .warn: return .yellow
        case .spent: return .red
        }
    }

    var symbol: String {
        switch self {
        case .ok: return "gauge.with.dots.needle.33percent"
        case .warn: return "gauge.with.dots.needle.67percent"
        case .spent: return "gauge.with.dots.needle.100percent"
        }
    }
}

struct GatewaySection: View {
    @EnvironmentObject var store: Store
    @EnvironmentObject var preferences: Preferences

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Toggle("Gateway", isOn: $preferences.gatewayOn).toggleStyle(.switch).controlSize(.small)
                    .onChange(of: preferences.gatewayOn) { store.apply() }
                Spacer()
                Text("port").font(.caption).foregroundStyle(.secondary)
                TextField("4141", value: $preferences.gatewayPort, format: .number.grouping(.never))
                    .textFieldStyle(.roundedBorder).frame(width: 64).font(.caption)
                    .onSubmit {
                        if !(1...65535).contains(preferences.gatewayPort) { preferences.gatewayPort = 4141 }
                        store.apply()
                    }
            }
            Text("Serves the Anthropic API on 127.0.0.1 as the account in use, for pi and Aside.")
                .font(.caption2).foregroundStyle(.secondary)
            DaemonLines(daemon: store.gateway)
        }
    }
}

struct RotationSection: View {
    @EnvironmentObject var store: Store
    @EnvironmentObject var preferences: Preferences

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Toggle("Rotate automatically", isOn: $preferences.rotationOn).toggleStyle(.switch).controlSize(.small)
                .onChange(of: preferences.rotationOn) { store.apply() }
            Text("Switch away from a pooled account whose session runs high, to the one whose weekly resets soonest.")
                .font(.caption2).foregroundStyle(.secondary)
            ForEach(store.accounts) { account in
                Toggle(account.email, isOn: Binding(
                    get: { preferences.pool.contains(account.slug) },
                    set: { on in
                        preferences.toggle(account.slug, inPool: on)
                        store.apply()
                    }))
                .toggleStyle(.checkbox).font(.caption)
            }
            Toggle("Notifications", isOn: $preferences.notificationsOn).toggleStyle(.switch).controlSize(.small)
                .padding(.top, 4)
            DaemonLines(daemon: store.watcher)
        }
    }
}

/// The last few lines a daemon printed, or why it is no longer running.
struct DaemonLines: View {
    @ObservedObject var daemon: Daemon

    var body: some View {
        if let exited = daemon.exitedWith {
            Text(exited).font(.caption2).foregroundStyle(.red).textSelection(.enabled)
        } else if daemon.running, let last = daemon.lines.last {
            Text(last).font(.caption2).foregroundStyle(.secondary).lineLimit(2).textSelection(.enabled)
        }
    }
}
