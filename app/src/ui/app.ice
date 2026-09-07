daemon Ccs
  title "ccs"
  id "dev.orthory.ccs"
  palette CcsTheme.dark
  window main
    size 440 680
    min-size 360 480
    position centered
  tray
    icon-rgba "../../assets/tray.rgba" 22 22
    icon-template true
    label "–"
    tooltip "ccs"
    menu
      "Show ccs" -> show
      separator
      "Quit" -> quit

use "theme.ice"

state
  main_window:window-id? = none

// The window is the app on every platform; the tray is the macOS way of
// getting at it without it.
on mount
  task window open main -> opened _

on opened(id)
  main_window = some(id)

on show
  task window open main -> opened _

on quit
  exit

view
  col w=fill h=fill p=16.0 gap=12.0 @bg-bg
    text "ccs" size=20.0 @text-fg font-bold
    text "accounts will be listed here" @text-muted
