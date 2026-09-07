// The window is the app on every platform; the tray is the macOS way of
// getting at it without it. The first reading is whatever the watcher last
// wrote down; the watcher itself starts here, once, and is the one thing
// that polls. What it rotates among and whether it notifies is set, not
// restarted: no turn is wasted on a toggle.
on mount
  watching = set_watch(pool_for(rotation_on, pool), notifications_on)
  parallel
    task window open main -> opened _
    run every load() -> loaded _ | failed _
    stream every watch(90.0) -> polled _ | poll_failed _
    stream every tray_clicks() -> show
    run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

// A window freshly opened is the window, and starts without a stale
// complaint on it.
on opened(id)
  main_window = some(id)
  error = ""

// The window, from the bar's icon or wherever else it is asked for: the
// one that is open comes forward, and one opens when none is. A handler
// has no branch, so the choice is an extern's answer.
on show
  run every raise(main_window) -> raised _ | open_main _

on raised(id)
  task window focus target=id

on open_main(_none)
  task window open main -> opened _

// The window closed by hand is forgotten, so the next click opens one; the
// daemon stays, on the bar.
on window_gone(id)
  return if !is_window(main_window, id)
  main_window = none

subscribe
  window closed with-id -> window_gone _

on loaded(next)
  accounts = next
  pool_rows_now = pool_rows(next, pool)
  error = ""

on failed(cause)
  error = cause.message

// A pick is a switch, unless the account is spent: then the switch comes
// back refused, and the refusal is what asks.
on pick(slug)
  return if empty(slug)
  error = ""
  run every switch(slug, false) -> switched _ | refused _

on switched(next)
  accounts = next
  pool_rows_now = pool_rows(next, pool)
  confirming = ""
  error = ""

on refused(cause)
  error = cause.message
  return if !cause.spent
  error = ""
  confirming = cause.slug
  run every raise(main_window) -> raised _ | open_main _

on confirm
  let slug = confirming
  confirming = ""
  return if empty(slug)
  run every switch(slug, true) -> switched _ | refused _

on cancel
  confirming = ""

// Every turn of the watcher lands here: the list as it stands, and what
// the turn did.
on polled(turn)
  accounts = turn.accounts
  pool_rows_now = pool_rows(turn.accounts, pool)
  watcher_line = watcher_said(turn.notices, turn.rotated)

on poll_failed(cause)
  watcher_line = cause.message

// The gateway follows its switch and its port; the watcher is told the
// pool and whether to notify. Every change is written down.
on toggle_gateway(on)
  gateway_on = on
  saved = save_prefs(on, gateway_port, rotation_on, pool, notifications_on, launch_at_login_on)
  run every gateway(on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

// The port field's submit. The field edits a draft, so a port half typed
// is never written down or tried by another toggle; one that is not a
// port is refused here, and the line under the switch says so.
on apply_gateway
  gateway_line = port_line(port_draft, gateway_on)
  return if !is_port(port_draft)
  gateway_port = port_draft
  saved = save_prefs(gateway_on, gateway_port, rotation_on, pool, notifications_on, launch_at_login_on)
  return if !gateway_on
  run every gateway(true, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on gateway_said(line)
  gateway_line = line

on gateway_failed(cause)
  gateway_on = false
  gateway_line = cause.message
  saved = save_prefs(false, gateway_port, rotation_on, pool, notifications_on, launch_at_login_on)

on toggle_rotation(on)
  rotation_on = on
  saved = save_prefs(gateway_on, gateway_port, on, pool, notifications_on, launch_at_login_on)
  watching = set_watch(pool_for(on, pool), notifications_on)
  run every gateway(gateway_on, gateway_port, pool_for(on, pool)) -> gateway_said _ | gateway_failed _

on pool_flipped(slug)
  pool = toggled(pool, slug, !in_pool(pool, slug))
  pool_rows_now = pool_rows(accounts, pool)
  saved = save_prefs(gateway_on, gateway_port, rotation_on, pool, notifications_on, launch_at_login_on)
  watching = set_watch(pool_for(rotation_on, pool), notifications_on)
  run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on toggle_notifications(on)
  notifications_on = on
  saved = save_prefs(gateway_on, gateway_port, rotation_on, pool, on, launch_at_login_on)
  watching = set_watch(pool_for(rotation_on, pool), on)

// Starting at login is the platform's to keep; what it kept is what shows.
on toggle_login(on)
  run every launch_at_login(on) -> login_set _ | login_failed _

on login_set(on)
  launch_at_login_on = on
  saved = save_prefs(gateway_on, gateway_port, rotation_on, pool, notifications_on, on)

on login_failed(cause)
  error = cause.message

// The gateway holds a port; it is let go before the process is.
on quit
  watching = shutdown()
  exit
