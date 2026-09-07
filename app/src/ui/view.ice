// One limit as a bar: its name, the bar tinted by its standing, the figure,
// and when it comes back.
component LimitBar(limit:Limit)
  row w=fill gap=8.0 align=center
    text limit.column w=72.0 @text-muted text-xs
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
component AccountRow(account:Account)
  col w=fill p=12.0 gap=6.0 @bg-surface rounded-lg
    row w=fill gap=8.0 align=center
      text mark(account.active) @text-primary
      text account.email @text-fg font-semibold
      text account.plan @text-muted text-sm
    if account.note != ""
      text account.note @text-muted text-xs
    for limit in account.limits
      LimitBar limit=limit

view
  col w=fill h=fill @bg-bg
    scroll #accounts w=fill h=fill
      col w=fill p=16.0 gap=10.0
        text "Accounts" size=18.0 @text-fg font-bold
        if error != ""
          text error @text-danger text-sm
        for account in accounts
          lazy account by account.slug, account.polled, account.active as held
            AccountRow account=held #account(held.slug)
