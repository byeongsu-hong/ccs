# `ccs-app`: the switcher as an Ice daemon, one process, every platform

## Goal

Replace the Swift menu bar app with a Rust one written in Ice (ducktape-ui),
so the app runs wherever ccs runs, links ccs as a library instead of shelling
out to it, and hosts the gateway and the watcher in its own process.

## What decides the shape

- Ice's tray is a real status item on macOS only; on other targets the
  runtime stubs it. So the window is the whole app on every platform, and the
  tray is a macOS accessory that shows the Claude session on the bar and
  offers the switches without opening the window.
- A tray menu's row count is fixed at compile time. Accounts get fixed slots
  (8 Claude, 4 Codex) guarded by `when`, each row's text composed by a `pure`
  extern; the window's list is a `for` and has no such limit.
- `ui-lang` is not on crates.io; the app takes it as a git dependency on
  `byeongsu-hong/ducktape-ui` pinned to a revision.
- ccs's core is Unix today (file modes, the notice socket, pen symlinks,
  `/dev/urandom`). This pass targets macOS and Linux; the app compiles on
  Windows once the core is ported, which is listed as follow-up work.

## ccs as a library

`src/lib.rs` exposes the modules the binary already has; `src/main.rs` keeps
the CLI on top of it. What the app needs from it:

- `Env`: an owned bundle of what `Ctx` borrows — backend, live stores, stash,
  usage cache, API clients, home — built by `Env::open()` from the same
  environment `main` reads, with `env.ctx()` handing out the borrowed view
  every command takes. The credential store trait objects become
  `Send + Sync` (the stores are plain data) so an `Arc<Env>` can be shared
  with the executor's threads.
- `cmd::readings(ctx) -> Vec<Cached>` (today's `cached_entries`, public), and
  `cmd::poll(ctx, &mut Snapshot, high, pool) -> Poll { entries, events,
  rotated }`: one turn of the watch loop, which `ccs watch` now calls in its
  loop and the app calls from a stream.
- `cmd::switch(ctx, slug, force)`, the body of `ccs use` without the printing.
- `serve::listen` as it is, plus `cmd::desk(ctx, pool, inbox)`: the loop the
  serve command runs on its main thread, callable on a thread of the app's.

Every command still works from the terminal; the library is the same code
reached without a process boundary.

## The app

`app/`, crate `ccs-app`, a workspace member beside the root package. Layout:

```
app/
  Cargo.toml              iced =0.14.0, ui-lang* by git rev, ccs by path, notify-rust
  build.rs                ui_lang_build::compile_dir("src/ui")
  Info.plist              the macOS bundle, as before
  src/main.rs             include_app!("src/ui/app.ice"); Env::open into a global
  src/backend.rs          the extern namespace: structs, async load/switch/apply,
                          sync preferences, the watcher stream, gateway control
  src/format.rs           pure formatters: rows, labels, countdowns
  src/platform.rs         notifications and launch-at-login, cfg per OS
  src/ui/app.ice          daemon root: window, tray, use lines
  src/ui/theme.ice        theme contract and palettes (light, dark)
  src/ui/externs.ice      extern declarations
  src/ui/state.ice        state, derived
  src/ui/handlers.ice     on handlers, subscribe
  src/ui/view.ice         the window
  src/ui/tests.ice        first-class tests
  assets/tray.rgba        22x22 template icon
```

### Data

```
extern crate::backend
  Limit(column:str, percent:f64, resets_in:str, health:str)
  Account(provider:str, slug:str, email:str, plan:str, active:bool,
          spent:bool, session_percent:f64, polled_at:str, polled:str,
          note:str, limits:[Limit])
  Prefs(gateway_on:bool, gateway_port:str, rotation_on:bool,
        pool:[str], notifications_on:bool, launch_at_login:bool)
  Poll(accounts:[Account], notices:[str], rotated:[str])
  PoolEntry(slug:str, email:str, ticked:bool)
  Failure(message:str, slug:str, spent:bool)
```

`health` is `ok`, `warn` or `spent`, decided in Rust by the same thresholds
the CLI uses. `resets_in` is the CLI's countdown string.

### Externs

- `load() -> [Account] ! Failure`: the cache, via `readings`. Never polls.
- `switch(slug:str, force:bool) -> [Account] ! Failure`: `cmd::switch`, then
  the cache again.
- `stream watch(high:f64) -> Poll ! Failure`: one thread for the life of
  the process, a `cmd::poll` every five minutes, each turn yielded; it
  reads the pool and whether to notify fresh each turn from what `sync
  set_watch(pool, notify)` last set, so a toggle never restarts it, wastes
  a poll, or lets an old turn rotate on a pool that was just changed. A
  first turn is skipped while the cache is younger than the interval.
- `gateway(on:bool, port:str, pool:[str]) -> str ! Failure`: starts or stops
  the listener and desk threads; the string is the log line for the window.
- `sync pref_gateway_on()`, `pref_gateway_port()`, `pref_rotation_on()`,
  `pref_pool()`, `pref_notifications_on()`, `pref_launch_at_login()`, and
  `sync save_prefs(...) -> bool` taking every field: a JSON file under the
  stash root, `ccs/app.json`. One accessor per field because a state
  initializer cannot project a field off a call, and Ice cannot construct
  a struct to hand back.
- Notifications are shown from the watcher thread itself, through
  `notify-rust`, since a handler cannot walk a list of notices.
- `launch_at_login(on:bool) -> bool ! Failure`: LaunchAgent plist on macOS,
  XDG autostart entry on Linux; returns what it managed to set.
- `pure` formatters: `bar_label(accounts)` for the tray label, `row(accounts,
  index, provider)` and `has(accounts, index, provider)` for the slots,
  `daemon_line(on, port)`.

### State

`accounts:[Account]`, the preference fields one by one, `error`,
`confirming` (the slug a spent account's confirmation is up for, or empty),
`gateway_line`, `watcher_line`, `pool_rows_now`, `saved`, `watching`. The
window's id is the platform's; nothing here reads it.

### Handlers

`mount`: read prefs, `load`, open the window, start the watcher stream, start
the gateway if on. `loaded`/`failed`. `pick(slug)`: switch, or set
`confirming` when the account is spent. `confirm`/`cancel`. `polled(next)`:
replace accounts, notify for each notice when notifications are on.
`toggle_gateway`, `apply_gateway` (the port field's submit),
`toggle_rotation`, `pool_flipped(slug)` (the tick emits its account and the
handler flips its place, since a checkbox route that names a lazy row's
field is generated as two moves), `toggle_notifications`, `toggle_login`:
update prefs, save, apply (restart the watcher stream with the new pool,
start/stop the gateway).
`show`: open the window. `quit`: stop the gateway (a `sync` call: it flips
a flag and knocks once), `exit`.

Every handler call that blocks — a load, a switch, the gateway going up or
down, the login entry — runs on a thread of its own and is awaited, so
iced's one executor thread is never held on a keychain read or a probe.
`STASH` serialises the whole-stash operations among those threads and the
watcher; the gateway's desk thread relies on the file lock the CLI already
shares between `ccs serve` and `ccs watch`.

### View

One `scroll` column: the accounts as rows (email, plan, active mark, a
`progress` per limit with its column name, percent and countdown, tinted by
health; a spent inactive row dimmed), then the gateway card (toggler, port
input, last line), rotation card (toggler, a checkbox per account), the
notifications and launch-at-login togglers, the polled stamp, quit. The
confirmation is an `overlay` when `confirming` is set.

### Tray (macOS)

`label bar_label(accounts)`; menu: 8 Claude slots and 4 Codex slots as
`row(accounts, i, "claude") -> pick_claude_i when has(accounts, i,
"claude")`, a separator, `daemon_line(prefs.gateway_on, prefs.gateway_port)
-> toggle_gateway`, the rotation toggle likewise, "Show ccs" -> show,
separator, "Quit" -> quit. The icon is a template so it reads on both bars.

## Packaging

`make app` builds `ccs-app` in release and wraps it into `build/ccs.app`
with the existing `Info.plist` on macOS, or copies the binary and a
`ccs.desktop` into `build/` on Linux. `make install-app` installs either.
`make check` gains `cargo ice check` for `app/`. The Swift package is deleted.

## Tests

- Ice: choosing a tray slot switches; a stat row is not a command; the window
  lists every account and the pick of a spent account asks first; toggling
  the gateway changes the daemon line.
- Rust: `Env::open` builds against a temp home; formatters; `cmd::poll` over a
  fixture (no network; probes return errors and the events are empty).
- The daemon is launched by hand on macOS and Linux (a VM or CI runner).

## Out of scope

Windows support of the core; a WebSocket relay; code signing.
