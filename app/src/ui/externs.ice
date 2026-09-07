// The typed boundary. Rust owns the stash, the network and the platform;
// the program here reads what it is handed.
extern crate::backend
  Limit(column:str, percent:f64, resets_in:str, health:str)
  Account(provider:str, slug:str, email:str, plan:str, active:bool, spent:bool, session_percent:f64, polled:str, note:str, limits:[Limit])
  Failure(message:str, slug:str, spent:bool)
  load() -> [Account] ! Failure
  switch(slug:str, force:bool) -> [Account] ! Failure
  sync fixture_accounts() -> [Account]

extern crate::format
  pure bar_label(accounts:&[Account]) -> str
  pure row(accounts:&[Account], index:i64, provider:str) -> str
  pure has(accounts:&[Account], index:i64, provider:str) -> bool
  pure slot(accounts:&[Account], index:i64, provider:str) -> str
  pure percent_label(percent:f64) -> str
  pure mark(active:bool) -> str
  pure active_of(accounts:&[Account], slug:str) -> bool
  pure confirm_question(accounts:&[Account], slug:str) -> str
