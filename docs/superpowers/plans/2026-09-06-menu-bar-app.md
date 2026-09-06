# ccs.app Menu Bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A macOS menu bar app that shows every stashed account's limits, switches on a click, and keeps `ccs serve` / `ccs watch` running with notifications.

**Architecture:** A SwiftUI `MenuBarExtra` app that is a thin shell over the installed `ccs` binary: `ccs ls --json` for readings, `ccs use` to switch, and `ccs serve` / `ccs watch` as owned child processes whose stdout is parsed for notices. Pure logic (decoding, health, arguments, notice parsing, binary lookup) is in plain Swift files with XCTest coverage; views and process spawning stay thin.

**Tech Stack:** Swift 5.9 package, SwiftUI `MenuBarExtra`, `Foundation.Process`, `UserNotifications`, `ServiceManagement`. macOS 14+.

**Spec:** `docs/superpowers/specs/2026-09-06-menu-bar-app-design.md`

## Global Constraints

- Minimum macOS 14; no Xcode project, `swift build` only.
- Bundle id `dev.orthory.ccs`, executable `ccs-menu`, bundle `ccs.app`, `LSUIElement` true.
- The app never reads credentials or the stash; every fact comes from the `ccs` binary.
- Binary lookup order: `CCS_BINARY`, `~/.cargo/bin/ccs`, `/usr/local/bin/ccs`, `/opt/homebrew/bin/ccs`. Never `PATH`.
- Health thresholds match the CLI: warn at 80, spent at 100, API severity wins below 100.
- Settings keys: `gateway.on`, `gateway.port` (4141), `rotation.on`, `rotation.pool`, `notifications.on`, `refresh.seconds` (300).

---

### Task 1: Package scaffold and the model

**Files:**
- Create: `app/Package.swift`, `app/Sources/CcsMenu/Model.swift`, `app/Sources/CcsMenu/main.swift` (placeholder, replaced in Task 5)
- Test: `app/Tests/CcsMenuTests/ModelTests.swift`

**Interfaces:**
- Produces: `struct Account: Decodable, Identifiable { slug, email, plan, active, limits }`, `struct Limit: Decodable { kind, percent, severity, resetsAt: Date?, column, health }`, `enum Health { ok, warn, spent }`, `func decodeAccounts(_ data: Data) throws -> [Account]`, `func until(_ date: Date, now: Date) -> String`.

- [ ] **Step 1: Package.swift**

```swift
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "CcsMenu",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "ccs-menu", targets: ["CcsMenu"])],
    targets: [
        .executableTarget(name: "CcsMenu", path: "Sources/CcsMenu"),
        .testTarget(name: "CcsMenuTests", dependencies: ["CcsMenu"], path: "Tests/CcsMenuTests"),
    ]
)
```

- [ ] **Step 2: Failing tests** in `ModelTests.swift`: decode the `ccs ls --json` shape (slug, email, plan, active, limits with a scoped model), columns (`session`, `weekly`, model name, unknown kind spaced), health thresholds (100 spent whatever severity; severity wins under 100; 80 warns without severity), `resets_at` with six fractional digits parses, `until` renders `3h54m` / `2d11h` / `now`.
- [ ] **Step 3: Run `swift test --package-path app`** — fails to compile (no `Account`).
- [ ] **Step 4: Implement Model.swift** (CodingKeys for snake_case, fractional-second-tolerant date parsing, `column`, `health`, `until`).
- [ ] **Step 5: Run tests — pass. Commit** `feat(app): package scaffold and the model of a reading`.

### Task 2: Finding and running ccs

**Files:**
- Create: `app/Sources/CcsMenu/Ccs.swift`
- Test: `app/Tests/CcsMenuTests/CcsTests.swift`

**Interfaces:**
- Produces: `func ccsCandidates(environment: [String: String], home: URL) -> [URL]`, `struct Ccs { let binary: URL; static func locate() -> Ccs?; func list() async throws -> [Account]; func use(_ slug: String, force: Bool) async throws -> String }`, `struct CcsError: LocalizedError { message }`.

- [ ] **Step 1: Failing tests**: candidates order with and without `CCS_BINARY`; `Ccs(binary: "/bin/sh"-style fake script)` `list()` decodes stdout; non-zero exit surfaces stderr's message with the `ccs: ` prefix stripped.
- [ ] **Step 2: Run — fails.** **Step 3: Implement** (`Process` with pipes, run off the main actor, `CcsError`). **Step 4: Pass. Commit** `feat(app): find and run the ccs binary`.

### Task 3: Daemon arguments and notices

**Files:**
- Create: `app/Sources/CcsMenu/Notices.swift`
- Test: `app/Tests/CcsMenuTests/NoticesTests.swift`

**Interfaces:**
- Produces: `enum Notice: Equatable { case sessionHigh(String), sessionReset(String), weeklyReset(String), rotated(String) }`, `func parseNotice(_ line: String) -> Notice?`, `func serveArguments(port: Int, pool: [String]) -> [String]`, `func watchArguments(pool: [String]) -> [String]`.

- [ ] **Step 1: Failing tests**: `"12:00:01 session-high: you@x has crossed 90%"` → `.sessionHigh("you@x has crossed 90%")`; `rotate: switched to a (b)` → `.rotated`; `poll failed: …` → nil; a bare `serve` log line → nil. Arguments: `serve --port 4141` alone, `--rotate a,b` appended when the pool is non-empty; `watch` likewise.
- [ ] **Step 2–4: fail, implement, pass. Commit** `feat(app): read the watcher's notices and spell the daemons' arguments`.

### Task 4: Settings, daemons and the store

**Files:**
- Create: `app/Sources/CcsMenu/Settings.swift`, `app/Sources/CcsMenu/Daemon.swift`, `app/Sources/CcsMenu/Store.swift`

**Interfaces:**
- Produces: `final class Settings: ObservableObject` (published `gatewayOn`, `gatewayPort`, `rotationOn`, `pool: [String]`, `notificationsOn`, `refreshSeconds`, backed by `UserDefaults`); `final class Daemon: ObservableObject` (`running`, `lines`, `start(binary:arguments:onLine:)`, `stop()`); `@MainActor final class Store: ObservableObject` (`accounts`, `updatedAt`, `error`, `ccs`, `gateway`, `watcher`, `refresh()`, `switchTo(_:force:)`, `apply()` which reconciles daemons with settings, `notify(_:)`).

- [ ] Implement, `swift build` clean. Not unit-tested (process spawning); exercised by hand in Task 6. **Commit** `feat(app): settings, owned daemons and the store`.

### Task 5: Views and the app

**Files:**
- Create: `app/Sources/CcsMenu/CcsMenuApp.swift` (replaces `main.swift`), `app/Sources/CcsMenu/Views.swift`

- [ ] `MenuBarExtra` window style; label = active session percent with a gauge symbol by health; popover with accounts (rows, bars, click to switch with confirmation when spent), gateway section (toggle, port, last lines), rotation section (toggle, pool checkboxes), notifications toggle, launch-at-login toggle, refresh + stamp, quit. `swift build` clean. **Commit** `feat(app): the menu bar item and its popover`.

### Task 6: Bundle, Makefile, README

**Files:**
- Create: `app/Info.plist`; Modify: `Makefile`, `README.md`, `.gitignore` (`app/.build`, `build/`)

- [ ] `make app` → `build/ccs.app` (swift build release, copy binary, plist, ad-hoc codesign); `make install-app` / `uninstall-app`; `make test-app`. README section "In the menu bar". Launch the bundle by hand, verify: readings, switch, gateway on (Aside request goes through), watcher on, a notification arrives, quit kills children. **Commit** `feat(app): build ccs.app with make`.

## Self-review

Spec coverage: item label (T5), popover accounts/switch/confirm (T5), gateway (T4/T5), rotation pool (T4/T5), notifications (T3/T4), launch at login (T5), settings keys (T4), binary lookup (T2), errors (T4/T5), bundle/Makefile (T6), tests (T1–T3). Types named consistently across tasks.
