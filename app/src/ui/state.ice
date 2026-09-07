state
  accounts:[Account] = []
  error = ""
  // The account a switch is waiting on a yes for, or nothing.
  confirming = ""
  // The daemons, as the window has them set. Persisted from Task 5 on.
  gateway_on = false
  gateway_port = "4141"
  gateway_line = "off"
  rotation_on = false
  pool:[str] = []
  notifications_on = true
  watcher_line = ""
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
