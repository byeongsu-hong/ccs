# `ccs.app`: the switcher in the menu bar

## Goal

Everything `ccs` shows in its picker, and the two daemons it can run, from a
menu bar icon: see what every account has left, switch with a click, keep the
gateway (`ccs serve`) and the watcher (`ccs watch --rotate`) running for as long
as the app is, and hear about a session running high as a macOS notification.

## Shape

A thin shell over the installed `ccs` binary. The app never reads credentials
or the stash itself, and never polls: it runs `ccs ls --cached --json` for the
readings the watcher last wrote down, `ccs use <slug>` to switch, and owns
`ccs serve` and `ccs watch` as child processes. The watcher always runs — it
is the one poller, and the cache it writes is the source of truth — so the
readings cost nothing however often the popover is opened. One
implementation of token refresh, keychain and reconciliation, and the app
inherits every fix to it.

## Layout

```
app/
  Package.swift                 executable target ccs-menu, test target
  Sources/CcsMenu/
    CcsMenuApp.swift            @main, MenuBarExtra, settings storage
    Model.swift                 Account, Limit, columns, Health
    Ccs.swift                   finding and running the binary
    Daemons.swift               serve / watch children, log lines, notices
    Views/…                     popover, account row, bars, sections
  Tests/CcsMenuTests/           decoding, health, notices, arguments, lookup
Makefile                        app, install-app, uninstall-app targets
```

Minimum macOS 14. No Xcode project: `swift build` produces the executable and
`make app` wraps it in `build/ccs.app` with an `Info.plist` carrying
`LSUIElement` (no Dock icon) and the bundle id `dev.orthory.ccs`, which
notifications require. `make install-app` copies it to `/Applications`.

## The menu bar item

`MenuBarExtra` in window style. The label is the active account's session
percentage, coloured by the same thresholds the CLI uses: green under 80,
yellow from 80, red at 100. `–` when no reading has arrived yet, and the
percentage dimmed when the last refresh failed and the number shown is stale.

## The popover

- **Accounts.** One row per stashed account, ordered as `ccs ls` orders them:
  email, plan, and a bar each for session, weekly and every model-scoped
  limit the endpoint reports, with percent and "resets in". The active
  account is marked. Clicking a row runs `ccs use <slug>`; an account with
  any limit at 100% asks first, as the picker does. A refresh button and a
  "polled N min ago" stamp, the newest reading's own time; a red line with `ccs`'s own message when the
  last run failed, and the install hint when the binary is missing.
- **Gateway.** A toggle and a port field. On, the app spawns
  `ccs serve --port <port>`, adding `--rotate <pool>` when a pool is set,
  and shows the last few log lines. Off, it terminates the child. The child
  is killed when the app quits.
- **Rotation.** A toggle and a checkbox per stashed account forming the
  pool. On, the app spawns `ccs watch --rotate <pool>` (`--every 300
  --high 90`, the CLI's defaults). Changing the pool restarts the watcher
  and, when it is running, the gateway.
- **Notifications.** A toggle. The app reads the watcher's stdout and turns
  lines of the kinds `session-high`, `session-reset`, `weekly-reset` and
  `rotate:` into notifications through `UNUserNotificationCenter`. The
  watcher runs whether or not this is on; the toggle only decides what is
  done with what it says.
- **Launch at login.** A toggle over `SMAppService.mainApp`.
- **Quit.**

## Data

`ccs ls --cached --json` is an array of `{slug, email, plan, active,
polled_at, limits: [{kind, percent, severity, resets_at, scope: {model:
{display_name}}}]}`, with `error` in place of readings for an account nothing
has polled yet. Columns follow the CLI: `session` and `weekly_all` under those
names, a scoped limit under its model's display name, anything else under its
kind. The cache is re-read every thirty seconds, when the popover opens, after
a switch, and whenever the watcher prints a line (each follows a poll).

Settings live in `UserDefaults`: `gateway.on`, `gateway.port` (4141),
`rotation.on`, `rotation.pool` (slugs), `notifications.on`,
`refresh.seconds` (30, the cache re-read; never under 15), and
`children.pids`, the daemons the last run started.

## Finding `ccs`

`CCS_BINARY` from the environment, then `~/.cargo/bin/ccs`,
`/usr/local/bin/ccs`, `/opt/homebrew/bin/ccs`. A desktop-launched app does not
inherit a shell's `PATH`, so nothing here searches it.

## Errors

A failed `ccs ls` leaves the last reading up and says so; a failed switch
shows the message inline and re-reads. A child that exits on its own keeps
its toggle as it was and shows why it died under it — its last line when the
exit status was not zero — so a port already in use or a missing pool account
is seen. The next timer tick starts it again, since the watcher is where
every reading comes from; a child that keeps dying is therefore retried every
half minute with its reason on screen, never in a tight loop. An app that
crashed never reaches its shutdown, so the pids of its children are recorded,
and the next launch takes down any of them still running `ccs watch` or
`ccs serve` before starting its own.

## Testing

`swift test` covers the pure parts: decoding a `ccs ls --json` document into
rows and columns, health thresholds, parsing watcher lines into notice kinds,
building `serve` and `watch` argument lists from settings, and the binary
lookup order. Views and process spawning stay thin and are exercised by hand.

## Out of scope

Pins, `ccs add`, removing accounts, editing the stash, code signing and
notarisation.
