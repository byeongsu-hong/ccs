// The switcher in the menu bar: the active account's session percentage on
// the bar, everything else in the popover.

import SwiftUI

@main
struct CcsMenuApp: App {
    @NSApplicationDelegateAdaptor private var delegate: Delegate
    @StateObject private var preferences: Preferences
    @StateObject private var store: Store

    init() {
        let preferences = Preferences()
        let store = Store(preferences: preferences)
        _preferences = StateObject(wrappedValue: preferences)
        _store = StateObject(wrappedValue: store)
        Delegate.store = store
    }

    var body: some Scene {
        MenuBarExtra {
            PopoverView()
                .environmentObject(store)
                .environmentObject(preferences)
        } label: {
            Label {
                Text(store.label).monospacedDigit()
            } icon: {
                Image(systemName: store.labelHealth.symbol)
            }
            .labelStyle(.titleAndIcon)
        }
        .menuBarExtraStyle(.window)
    }
}

/// Owns what has to happen outside SwiftUI: the app is an accessory (no
/// Dock icon, whichever way it was launched), and the daemons die with it.
final class Delegate: NSObject, NSApplicationDelegate {
    @MainActor static var store: Store?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.accessory)
    }

    func applicationWillTerminate(_ notification: Notification) {
        Task { @MainActor in Self.store?.shutdown() }
    }
}
