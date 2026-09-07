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
      // A menu's rows are fixed when the program compiles, so accounts get
      // slots: enough of each provider, each one absent while it is empty.
      // A row is its account's whole readout, and pressing it is the switch.
      row(accounts, 0, "claude") -> pick_claude_0 when has(accounts, 0, "claude")
      row(accounts, 1, "claude") -> pick_claude_1 when has(accounts, 1, "claude")
      row(accounts, 2, "claude") -> pick_claude_2 when has(accounts, 2, "claude")
      row(accounts, 3, "claude") -> pick_claude_3 when has(accounts, 3, "claude")
      row(accounts, 4, "claude") -> pick_claude_4 when has(accounts, 4, "claude")
      row(accounts, 5, "claude") -> pick_claude_5 when has(accounts, 5, "claude")
      row(accounts, 6, "claude") -> pick_claude_6 when has(accounts, 6, "claude")
      row(accounts, 7, "claude") -> pick_claude_7 when has(accounts, 7, "claude")
      separator
      row(accounts, 0, "codex") -> pick_codex_0 when has(accounts, 0, "codex")
      row(accounts, 1, "codex") -> pick_codex_1 when has(accounts, 1, "codex")
      row(accounts, 2, "codex") -> pick_codex_2 when has(accounts, 2, "codex")
      row(accounts, 3, "codex") -> pick_codex_3 when has(accounts, 3, "codex")
      separator
      // The two switches, each row saying what it is doing and what pressing
      // it does.
      gateway_row(gateway_on, gateway_port) -> flip_gateway
      rotation_row(rotation_on) -> flip_rotation
      separator
      "Show ccs" -> show
      "Quit" -> quit

use "theme.ice"
use "externs.ice"
use "state.ice"
use "handlers.ice"
use "view.ice"
use "tests.ice"
