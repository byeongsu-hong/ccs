// Picking an account is a switch. The fixture answers the way the stash
// would, so what is asserted is the program's wiring: the pick reaches the
// switch, the switch's answer reaches the list, and the marker moves.
test picking_an_account_switches_to_it
  preset seeded
  expect active_of(accounts, "hong")
  dispatch pick("agent")
  expect active_of(accounts, "agent")
  expect !active_of(accounts, "hong")
  expect active_of(accounts, "codex-frost")

// A spent account is not walked into: the switch comes back refused, the
// question goes up, and only a yes goes through.
test picking_a_spent_account_asks_first
  preset seeded
  dispatch pick("robin")
  expect confirming == "robin"
  expect !active_of(accounts, "robin")
  expect error == ""
  dispatch confirm
  expect active_of(accounts, "robin")
  expect confirming == ""

test a_no_leaves_the_account_where_it_was
  preset seeded
  dispatch pick("robin")
  dispatch cancel
  expect confirming == ""
  expect active_of(accounts, "hong")

// The tray's slots reach the same switch through the same fixture, and are
// laid out in listing order per provider.
test choosing_a_tray_slot_switches_to_its_account
  preset seeded
  expect tray label "52%"
  expect tray command "○ agent@example.com · max20x · session 6% · weekly 37%"
  tray choose "○ agent@example.com · max20x · session 6% · weekly 37%"
  expect active_of(accounts, "agent")
  expect tray label "6%"

// The Codex slot sits in its own group under the Claude ones, and a slot
// with no account behind it is guarded out of the menu.
test each_provider_has_its_own_slots
  preset seeded
  expect tray command "● codex-frost@example.com · codex pro · session 0% · weekly 37%"
  expect no tray item "○ codex-frost@example.com"
