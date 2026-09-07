// The window is the app on every platform; the tray is the macOS way of
// getting at it without it. The first reading is whatever the watcher last
// wrote down; the watcher itself starts here, once, and is the one thing
// that polls. What it rotates among and whether it notifies is set, not
// restarted: no turn is wasted on a toggle.
on mount
  watching = set_watch(pool_for(rotation_on, pool), notifications_on)
  parallel
    task window open main -> opened
    run every load() -> loaded _ | failed _
    stream every watch(90.0) -> polled _ | poll_failed _
    run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

// A window freshly opened starts without a stale complaint on it.
on opened
  error = ""

on show
  task window open main -> opened

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
  task window open main -> opened

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

on flip_gateway
  gateway_on = !gateway_on
  saved = save_prefs(gateway_on, gateway_port, rotation_on, pool, notifications_on, launch_at_login_on)
  run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

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

on flip_rotation
  rotation_on = !rotation_on
  saved = save_prefs(gateway_on, gateway_port, rotation_on, pool, notifications_on, launch_at_login_on)
  watching = set_watch(pool_for(rotation_on, pool), notifications_on)
  run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

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

// The tray's slots. A menu row carries no payload, so each slot has a
// handler of its own that reads its account off the same list the row's
// text was composed from.
on pick_claude_0
  error = ""
  run every switch(slot(accounts, 0, "claude"), false) -> switched _ | refused _

on pick_claude_1
  error = ""
  run every switch(slot(accounts, 1, "claude"), false) -> switched _ | refused _

on pick_claude_2
  error = ""
  run every switch(slot(accounts, 2, "claude"), false) -> switched _ | refused _

on pick_claude_3
  error = ""
  run every switch(slot(accounts, 3, "claude"), false) -> switched _ | refused _

on pick_claude_4
  error = ""
  run every switch(slot(accounts, 4, "claude"), false) -> switched _ | refused _

on pick_claude_5
  error = ""
  run every switch(slot(accounts, 5, "claude"), false) -> switched _ | refused _

on pick_claude_6
  error = ""
  run every switch(slot(accounts, 6, "claude"), false) -> switched _ | refused _

on pick_claude_7
  error = ""
  run every switch(slot(accounts, 7, "claude"), false) -> switched _ | refused _

on pick_codex_0
  error = ""
  run every switch(slot(accounts, 0, "codex"), false) -> switched _ | refused _

on pick_codex_1
  error = ""
  run every switch(slot(accounts, 1, "codex"), false) -> switched _ | refused _

on pick_codex_2
  error = ""
  run every switch(slot(accounts, 2, "codex"), false) -> switched _ | refused _

on pick_codex_3
  error = ""
  run every switch(slot(accounts, 3, "codex"), false) -> switched _ | refused _

// The gateway holds a port; it is let go before the process is.
on quit
  watching = shutdown()
  exit
