// One limit as a bar: its name, the bar tinted by its standing, the figure,
// and when it comes back.
component LimitBar(limit:Limit)
  row w=fill gap=8.0 align=center
    text limit.column w=96.0 @text-muted text-xs
    match limit.health
      "spent"
        progress limit.percent min=0.0 max=100.0 girth=6.0 bar=danger bg=rail
      "warn"
        progress limit.percent min=0.0 max=100.0 girth=6.0 bar=warn bg=rail
      _
        progress limit.percent min=0.0 max=100.0 girth=6.0 bar=ok bg=rail
    text percent_label(limit.percent) w=40.0 @text-fg text-xs
    text limit.resets_in w=56.0 @text-muted text-xs

// One account: who, what plan, whether in use, and every limit it reports.
// The whole card is the switch.
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
        LimitBar limit=limit

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
              lazy account by account.slug, account.polled, account.active as held
                AccountRow account=held #account(held.slug) -> pick _
    layer
      col w=320.0 p=16.0 gap=12.0 @bg-surface rounded-lg
        text confirm_question(accounts, confirming) @text-fg
        row gap=8.0
          button "Switch anyway" #switch-anyway @bg-danger text-white px-12px py-8px rounded-md -> confirm
          button "Cancel" #cancel @bg-rail text-fg px-12px py-8px rounded-md -> cancel
