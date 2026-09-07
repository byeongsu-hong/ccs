// The window is the app on every platform; the tray is the macOS way of
// getting at it without it. The first reading is whatever the watcher last
// wrote down: nothing here polls.
on mount
  parallel
    task window open main -> opened _
    run every load() -> loaded _ | failed _

on opened(id)
  main_window = some(id)

on show
  task window open main -> opened _

on loaded(next)
  accounts = next
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
  confirming = ""
  error = ""

on refused(cause)
  error = cause.message
  return if !cause.spent
  error = ""
  confirming = cause.slug
  task window open main -> opened _

on confirm
  let slug = confirming
  confirming = ""
  return if empty(slug)
  run every switch(slug, true) -> switched _ | refused _

on cancel
  confirming = ""

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

on quit
  exit
