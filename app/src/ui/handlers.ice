// The window is the app on every platform; the tray is the macOS way of
// getting at it without it. The first reading is whatever the watcher last
// wrote down; the watcher itself starts here and is the one thing that
// polls.
on mount
  parallel
    task window open main -> opened
    run every load() -> loaded _ | failed _
    stream replace lane=watch watch(90.0, pool_for(rotation_on, pool), notifications_on) -> polled _ | poll_failed _

on show
  task window open main -> opened

// A window freshly opened starts without a stale complaint on it.
on opened
  error = ""

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
  error = ""

on poll_failed(cause)
  watcher_line = cause.message

// The daemons follow the switches. A change to the pool restarts the
// watcher with it and, when the gateway is up, the gateway too.
on toggle_gateway(on)
  gateway_on = on
  run every gateway(on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on flip_gateway
  gateway_on = !gateway_on
  run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on apply_gateway
  return if !gateway_on
  run every gateway(true, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on gateway_said(line)
  gateway_line = line

on gateway_failed(cause)
  gateway_on = false
  gateway_line = cause.message

on toggle_rotation(on)
  rotation_on = on
  parallel
    stream replace lane=watch watch(90.0, pool_for(on, pool), notifications_on) -> polled _ | poll_failed _
    run every gateway(gateway_on, gateway_port, pool_for(on, pool)) -> gateway_said _ | gateway_failed _

on flip_rotation
  rotation_on = !rotation_on
  parallel
    stream replace lane=watch watch(90.0, pool_for(rotation_on, pool), notifications_on) -> polled _ | poll_failed _
    run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on pool_flipped(slug)
  pool = toggled(pool, slug, !in_pool(pool, slug))
  pool_rows_now = pool_rows(accounts, pool)
  parallel
    stream replace lane=watch watch(90.0, pool_for(rotation_on, pool), notifications_on) -> polled _ | poll_failed _
    run every gateway(gateway_on, gateway_port, pool_for(rotation_on, pool)) -> gateway_said _ | gateway_failed _

on toggle_notifications(on)
  notifications_on = on
  stream replace lane=watch watch(90.0, pool_for(rotation_on, pool), on) -> polled _ | poll_failed _

// The tray's slots. A menu row carries no payload, so each slot has a
// handler of its own that reads its account off the same list the row's
// text was composed from.
on pick_claude_0
  run every switch(slot(accounts, 0, "claude"), false) -> switched _ | refused _

on pick_claude_1
  run every switch(slot(accounts, 1, "claude"), false) -> switched _ | refused _

on pick_claude_2
  run every switch(slot(accounts, 2, "claude"), false) -> switched _ | refused _

on pick_claude_3
  run every switch(slot(accounts, 3, "claude"), false) -> switched _ | refused _

on pick_claude_4
  run every switch(slot(accounts, 4, "claude"), false) -> switched _ | refused _

on pick_claude_5
  run every switch(slot(accounts, 5, "claude"), false) -> switched _ | refused _

on pick_claude_6
  run every switch(slot(accounts, 6, "claude"), false) -> switched _ | refused _

on pick_claude_7
  run every switch(slot(accounts, 7, "claude"), false) -> switched _ | refused _

on pick_codex_0
  run every switch(slot(accounts, 0, "codex"), false) -> switched _ | refused _

on pick_codex_1
  run every switch(slot(accounts, 1, "codex"), false) -> switched _ | refused _

on pick_codex_2
  run every switch(slot(accounts, 2, "codex"), false) -> switched _ | refused _

on pick_codex_3
  run every switch(slot(accounts, 3, "codex"), false) -> switched _ | refused _

// The gateway holds a port; it is let go before the process is.
on quit
  run every shutdown() -> gone

on gone
  exit
