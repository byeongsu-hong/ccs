state
  accounts:[Account] = []
  error = ""
  // The account a switch is waiting on a yes for, or nothing.
  confirming = ""
  main_window:window-id? = none

// What a first-class test starts from: the fixture's accounts, one in use
// per provider and one of them spent.
preset seeded
  state
    accounts = fixture_accounts()
