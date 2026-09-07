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
  expect tray command "● codex-frost@example.com · codex pro · weekly 37%"
  expect no tray item "○ codex-frost@example.com"

// The gateway's switch reaches the gateway and its answer reaches the line
// under the switch; a port typed in is what the next start uses.
test the_gateway_switch_starts_and_stops_it
  preset seeded
  expect gateway_line == "off"
  dispatch toggle_gateway(true)
  expect gateway_on
  expect gateway_line == "serving http://127.0.0.1:4141 as the accounts in use"
  dispatch toggle_gateway(false)
  expect gateway_line == "off"
  expect tray command "Gateway off — serve on :4141"

// Without a preset the program boots as it does for real: the cache is
// read, the watcher starts once, and the gateway follows what was
// remembered. A turn of the watcher replaces the list and says what it
// did, and each row is stamped with its new reading.
test booting_reads_the_cache_starts_the_watcher_and_asks_the_gateway
  expect watching
  expect active_of(accounts, "agent")
  expect watcher_line == "session-high: hong@example.com has crossed 90% · switched to agent@example.com"
  expect gateway_line == "off"
  expect polled_line(accounts) == "polled 09:05"

// Rotation is a setting the watcher reads, not a restart of it.
test rotation_is_told_to_the_watcher
  preset seeded
  dispatch toggle_rotation(true)
  expect rotation_on
  expect watching
  expect tray item "Rotating automatically — stop"
  dispatch toggle_notifications(false)
  expect !notifications_on

// A refusal that is not about a spent account is a plain error.
test a_switch_refused_for_another_reason_is_an_error_not_a_question
  preset seeded
  dispatch pick("nobody")
  expect confirming == ""
  expect error != ""

// The port field's submit retries the gateway on the port as it stands;
// a port that is not one is refused before anything is written down.
test the_port_field_applies_to_a_gateway_that_is_on
  preset seeded
  dispatch apply_gateway
  expect gateway_line == "off"
  dispatch toggle_gateway(true)
  dispatch apply_gateway
  expect gateway_line == "serving http://127.0.0.1:4141 as the accounts in use"
  expect is_port("4199")
  expect !is_port("lots")
  expect !is_port("0")

// Ticking an account puts it in the pool, and only while rotation is on is
// the pool handed to the daemons.
test the_pool_is_ticked_per_account
  preset seeded
  dispatch pool_flipped("agent")
  dispatch pool_flipped("robin")
  expect in_pool(pool, "agent")
  expect in_pool(pool, "robin")
  expect empty(pool_for(false, pool))
  expect ticked_in(pool_rows_now, "robin")
  dispatch pool_flipped("agent")
  expect !in_pool(pool, "agent")
  expect !ticked_in(pool_rows_now, "agent")

// Starting at login is asked of the platform and what it kept is shown.
test launch_at_login_follows_the_platforms_answer
  preset seeded
  expect !launch_at_login_on
  dispatch toggle_login(true)
  expect launch_at_login_on
  expect saved
  dispatch toggle_login(false)
  expect !launch_at_login_on
