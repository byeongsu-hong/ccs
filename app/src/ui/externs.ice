// The typed boundary. Rust owns the stash, the network and the platform;
// the program here reads what it is handed.
extern crate::backend
  Limit(column:str, percent:f64, resets_in:str, health:str)
  Account(provider:str, slug:str, email:str, plan:str, active:bool, spent:bool, session_percent:f64, polled:str, note:str, limits:[Limit])
  Failure(message:str, slug:str, spent:bool)
  Poll(accounts:[Account], notices:[str], rotated:[str])
  PoolEntry(slug:str, email:str, ticked:bool)
  load() -> [Account] ! Failure
  switch(slug:str, force:bool) -> [Account] ! Failure
  stream watch(high:f64, pool:[str], notify:bool) -> Poll ! Failure
  gateway(on:bool, port:str, pool:[str]) -> str ! Failure
  shutdown() -> unit
  sync fixture_accounts() -> [Account]

extern crate::format
  pure bar_label(accounts:&[Account]) -> str
  pure row(accounts:&[Account], index:i64, provider:str) -> str
  pure has(accounts:&[Account], index:i64, provider:str) -> bool
  pure slot(accounts:&[Account], index:i64, provider:str) -> str
  pure percent_label(percent:f64) -> str
  pure mark(active:bool) -> str
  pure active_of(accounts:&[Account], slug:str) -> bool
  pure confirm_question(accounts:&[Account], slug:&str) -> str
  pure in_pool(pool:&[str], slug:str) -> bool
  pure toggled(pool:&[str], slug:str, on:bool) -> [str]
  pure pool_for(rotation_on:bool, pool:&[str]) -> [str]
  pure gateway_row(on:bool, port:str) -> str
  pure rotation_row(on:bool) -> str
  pure polled_line(accounts:&[Account]) -> str
  pure pool_rows(accounts:&[Account], pool:&[str]) -> [PoolEntry]
  pure watcher_said(notices:&[str], rotated:&[str]) -> str
