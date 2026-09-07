daemon Ccs
  title "ccs"
  id "dev.orthory.ccs"
  palette CcsTheme.dark
  window main
    size 440 680
    min-size 360 480
    position centered
  // The menu bar item is the window's handle: the session on the bar, and
  // the window on a click. It has no menu of its own — a click on the icon
  // is caught below the language, in `tray_clicks`, and opens the window.
  tray
    icon-rgba "../../assets/tray.rgba" 22 22
    icon-template true
    label bar_label(accounts)
    tooltip "ccs"

use "theme.ice"
use "externs.ice"
use "state.ice"
use "handlers.ice"
use "view.ice"
use "tests.ice"
