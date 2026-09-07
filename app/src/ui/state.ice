state
  accounts:[Account] = []
  error = ""
  // The account a switch is waiting on a yes for, or nothing.
  confirming = ""
  // The daemons, as the window has them set; remembered between launches.
  gateway_on:bool = pref_gateway_on()
  gateway_port:str = pref_gateway_port()
  gateway_line = "off"
  rotation_on:bool = pref_rotation_on()
  pool:[str] = pref_pool()
  notifications_on:bool = pref_notifications_on()
  launch_at_login_on:bool = pref_launch_at_login()
  watcher_line = ""
  saved = true
  // Each account with whether it is in the pool, for the rows that tick it:
  // a row under `lazy` sees only itself, so the tick travels with it. Kept
  // in step by every handler that moves the accounts or the pool.
  pool_rows_now:[PoolEntry] = []

// What a first-class test starts from: the fixture's accounts, one in use
// per provider and one of them spent.
preset seeded
  state
    accounts = fixture_accounts()
    pool_rows_now = pool_rows(fixture_accounts(), [])
