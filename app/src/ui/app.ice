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
    label bar_label(accounts)
    tooltip "ccs"
    menu
      "Show ccs" -> show
      separator
      "Quit" -> quit

use "theme.ice"
use "externs.ice"
use "state.ice"
use "handlers.ice"
use "view.ice"
