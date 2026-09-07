// One account: who, what plan, whether in use, and every limit it reports
// as a bar tinted by its standing. The whole card is the switch.
component AccountRow(account:Account) -> str
  button #pick -> emit(account.slug)
    with
      label=account.email
      w=fill
      p=12.0
      @bg-surface rounded-lg
    col w=fill gap=6.0
      row w=fill gap=8.0 align=center
        text mark(account.active) @text-primary
        text account.email @text-fg font-semibold
        text account.plan @text-muted text-sm
      if account.note != ""
        text account.note @text-muted text-xs
      for limit in account.limits
        row w=fill gap=8.0 align=center
          text limit.column w=96.0 @text-muted text-xs
          if limit.health == "spent"
            progress limit.percent min=0.0 max=100.0 girth=6.0 bar=danger bg=rail
          if limit.health == "warn"
            progress limit.percent min=0.0 max=100.0 girth=6.0 bar=warn bg=rail
          if limit.health == "ok"
            progress limit.percent min=0.0 max=100.0 girth=6.0 bar=ok bg=rail
          text percent_label(limit.percent) w=40.0 @text-fg text-xs
          text limit.resets_in w=56.0 @text-muted text-xs

// One account's tick in the rotation pool: the tick emits the account, and
// the handler flips its place in the pool.
component PoolRow(slug:str, email:str, ticked:bool) -> str
  checkbox email #tick checked=ticked -> emit(slug)

view
  overlay #confirm when=(confirming != "") dismiss=cancel
    content
      col w=fill h=fill @bg-bg
        scroll #accounts w=fill h=fill
          col w=fill p=16.0 gap=10.0
            text "Accounts" size=18.0 @text-fg font-bold
            if error != ""
              text error @text-danger text-sm
            for account in accounts
              lazy account by account.slug, account.polled_at, account.active as held
                AccountRow account=held #account(held.slug) -> pick _
            col w=fill p=12.0 gap=6.0 @bg-surface rounded-lg
              row w=fill gap=8.0 align=center
                toggler "Gateway" #gateway checked=gateway_on -> toggle_gateway _
                space w=fill
                text "port" @text-muted text-xs
                input "port" #port <-> port_draft w=72.0 submit=apply_gateway
              text "Serves the Anthropic and Codex APIs on 127.0.0.1 as the accounts in use, for pi and Aside." @text-muted text-xs
              text gateway_line #gateway-line @text-muted text-xs
            col w=fill p=12.0 gap=6.0 @bg-surface rounded-lg
              toggler "Rotate automatically" #rotation checked=rotation_on -> toggle_rotation _
              text "Switch away from a pooled account whose session runs high, to the one whose weekly resets soonest." @text-muted text-xs
              for entry in pool_rows_now
                lazy entry by entry.slug, entry.ticked as held
                  PoolRow slug=held.slug email=held.email ticked=held.ticked #pool(held.slug) -> pool_flipped _
              toggler "Notifications" #notifications checked=notifications_on -> toggle_notifications _
              text watcher_line #watcher-line @text-muted text-xs
            row w=fill gap=8.0 align=center
              toggler "Launch at login" #launch checked=launch_at_login_on -> toggle_login _
              space w=fill
              text polled_line(accounts) #polled @text-muted text-xs
              button "Quit" #quit @bg-rail text-fg px-12px py-8px rounded-md -> quit
            if !saved
              text "the preferences could not be written" @text-danger text-xs
    layer
      col w=320.0 p=16.0 gap=12.0 @bg-surface rounded-lg
        text confirm_question(accounts, confirming) @text-fg
        row gap=8.0
          button "Switch anyway" #switch-anyway @bg-danger text-white px-12px py-8px rounded-md -> confirm
          button "Cancel" #cancel @bg-rail text-fg px-12px py-8px rounded-md -> cancel
