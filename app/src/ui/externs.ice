// The typed boundary. Rust owns the stash, the network and the platform;
// the program here reads what it is handed.
extern crate::backend
  Limit(column:str, percent:f64, resets_in:str, health:str)
  Account(provider:str, slug:str, email:str, plan:str, active:bool, spent:bool, session_percent:f64, polled_at:str, polled:str, note:str, limits:[Limit])
  Failure(message:str, slug:str, spent:bool)
  Poll(accounts:[Account], notices:[str], rotated:[str])
  PoolEntry(slug:str, email:str, ticked:bool)
  load() -> [Account] ! Failure
  switch(slug:str, force:bool) -> [Account] ! Failure
  stream watch(high:f64) -> Poll ! Failure
  stream tray_clicks() -> unit
  raise(held:window-id?) -> window-id ! Failure
  sync set_watch(pool:[str], notify:bool) -> bool
  gateway(on:bool, port:str, pool:[str]) -> str ! Failure
  sync shutdown() -> bool
  launch_at_login(on:bool) -> bool ! Failure
  sync pref_gateway_on() -> bool
  sync pref_gateway_port() -> str
  sync pref_rotation_on() -> bool
  sync pref_pool() -> [str]
  sync pref_notifications_on() -> bool
  sync pref_launch_at_login() -> bool
  sync save_prefs(gateway_on:bool, gateway_port:str, rotation_on:bool, pool:[str], notifications_on:bool, launch_at_login:bool) -> bool
  sync fixture_accounts() -> [Account]

extern crate::format
  pure bar_label(accounts:&[Account]) -> str
  pure is_window(held:window-id?, id:window-id) -> bool
  pure percent_label(percent:f64) -> str
  pure mark(active:bool) -> str
  pure active_of(accounts:&[Account], slug:str) -> bool
  pure confirm_question(accounts:&[Account], slug:&str) -> str
  pure in_pool(pool:&[str], slug:str) -> bool
  pure toggled(pool:&[str], slug:str, on:bool) -> [str]
  pure pool_for(rotation_on:bool, pool:&[str]) -> [str]
  pure polled_line(accounts:&[Account]) -> str
  pure pool_rows(accounts:&[Account], pool:&[str]) -> [PoolEntry]
  pure watcher_said(notices:&[str], rotated:&[str]) -> str
  pure is_port(port:&str) -> bool
  pure port_line(port:&str, on:bool) -> str
  pure ticked_in(rows:&[PoolEntry], slug:str) -> bool
