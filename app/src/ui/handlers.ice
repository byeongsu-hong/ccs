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

on quit
  exit
