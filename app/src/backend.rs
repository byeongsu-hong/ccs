//! The Rust side of the app: what the Ice program calls across its typed
//! boundary. The stash, the network and the platform live behind it; the
//! program reads what it is handed.

#[cfg(not(test))]
use std::sync::{Arc, Mutex, OnceLock};

use ccs::cmd::Cached;
#[cfg(not(test))]
use ccs::env::Env;
use ccs::model::Health;
use ccs::render;
use iced::futures::Stream;
use jiff::Timestamp;

/// One limit as the view draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub column: String,
    pub percent: f64,
    /// The CLI's countdown, or nothing when the limit names no reset.
    pub resets_in: String,
    /// `ok`, `warn` or `spent`: the CLI's thresholds.
    pub health: String,
}

/// One account as the view draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub provider: String,
    pub slug: String,
    pub email: String,
    pub plan: String,
    pub active: bool,
    /// Any limit with nothing left.
    pub spent: bool,
    /// The five-hour window, or -1 when the account reports none.
    pub session_percent: f64,
    /// How long ago the watcher last read it, or empty.
    pub polled: String,
    /// Why there is no reading, when there is none.
    pub note: String,
    pub limits: Vec<Limit>,
}

/// One account's place in the rotation pool, for the row that ticks it.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolEntry {
    pub slug: String,
    pub email: String,
    pub ticked: bool,
}

/// What went wrong, for the window to say. A switch refused because the
/// account is spent says so, and names the account, so the window can ask
/// and come back with `force`.
#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
    pub slug: String,
    pub spent: bool,
}

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), slug: String::new(), spent: false }
    }

    fn spent(slug: &str, email: &str) -> Self {
        Self {
            message: format!("{email} has nothing left on one of its limits"),
            slug: slug.to_string(),
            spent: true,
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::new(error.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": "))
    }
}

/// Everything a command borrows, opened once for the life of the process
/// and shared with every thread that asks.
#[cfg(not(test))]
static ENV: OnceLock<Result<Arc<Env>, String>> = OnceLock::new();

/// The stash is one thing however many threads reach for it: the watcher's,
/// the gateway desk's, and the handlers'. Everything that reads it as a whole
/// or writes it goes through here.
#[cfg(not(test))]
static STASH: Mutex<()> = Mutex::new(());

#[cfg(not(test))]
fn env() -> Result<Arc<Env>, Failure> {
    let opened =
        ENV.get_or_init(|| Env::open().map(Arc::new).map_err(|e| Failure::from(e).message));
    opened.clone().map_err(Failure::new)
}

/// The accounts as the watcher last wrote them down. Never polls: opening
/// the window twenty times costs the limits nothing.
pub async fn load() -> Result<Vec<Account>, Failure> {
    #[cfg(test)]
    return fixture::load();
    #[cfg(not(test))]
    {
        let env = env()?;
        let _stash = STASH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let readings = ccs::cmd::readings(&env.ctx())?;
        Ok(accounts_of(&readings, Timestamp::now()))
    }
}

/// Switch to `slug`. A spent account is refused unless `force`, with a
/// failure that says so: off a terminal the CLI's guard cannot ask, so the
/// window asks instead and comes back with `force`. What comes back is the
/// cache read again.
pub async fn switch(slug: String, force: bool) -> Result<Vec<Account>, Failure> {
    #[cfg(test)]
    return fixture::switch(&slug, force);
    #[cfg(not(test))]
    {
        let env = env()?;
        let _stash = STASH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let ctx = env.ctx();
        let before = accounts_of(&ccs::cmd::readings(&ctx)?, Timestamp::now());
        if let Some(account) = before.iter().find(|a| a.slug == slug && a.spent && !force) {
            return Err(Failure::spent(&account.slug, &account.email));
        }
        ccs::cmd::switch(&ctx, &slug, force)?;
        Ok(accounts_of(&ccs::cmd::readings(&ctx)?, Timestamp::now()))
    }
}

/// One turn of the watcher, as the window takes it.
#[derive(Clone, Debug)]
pub struct Poll {
    pub accounts: Vec<Account>,
    /// The notices raised this turn, one line each.
    pub notices: Vec<String>,
    /// The accounts rotated onto, one line each.
    pub rotated: Vec<String>,
}

/// The watcher: one poll every five minutes, on a thread of its own, each
/// turn handed to the window as it happens. The one thing in the process
/// that asks the API. Notices are shown as notifications here, when asked,
/// since a handler cannot walk a list. Dropping the stream stops the thread
/// at its next tick.
pub fn watch(
    high: f64,
    pool: Vec<String>,
    notify: bool,
) -> impl Stream<Item = Result<Poll, Failure>> + Send + 'static {
    #[cfg(test)]
    {
        let _ = (high, pool, notify);
        return fixture::watch();
    }
    #[cfg(not(test))]
    {
        use iced::futures::SinkExt;
        let (mut tx, rx) = iced::futures::channel::mpsc::channel::<Result<Poll, Failure>>(4);
        std::thread::spawn(move || {
            let mut before = ccs::watch::Snapshot::new();
            loop {
                let turn = match env() {
                    Ok(env) => {
                        let _stash = STASH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        let ctx = env.ctx();
                        let turn = ccs::cmd::poll(&ctx, &mut before, high, &pool);
                        match turn.failed {
                            Some(why) => Err(Failure::new(why)),
                            None => {
                                let readings = ccs::cmd::readings(&ctx).map_err(Failure::from);
                                readings.map(|readings| Poll {
                                    accounts: accounts_of(&readings, Timestamp::now()),
                                    notices: turn
                                        .notices
                                        .iter()
                                        .map(|(event, _)| format!("{}: {}", event.kind, event.text))
                                        .collect(),
                                    rotated: turn
                                        .rotations
                                        .iter()
                                        .map(|r| match r {
                                            Ok(switched) => format!(
                                                "switched to {}",
                                                switched.target.account.email
                                            ),
                                            Err(why) => format!("rotate failed: {why}"),
                                        })
                                        .collect(),
                                })
                            }
                        }
                    }
                    Err(failure) => Err(failure),
                };
                if notify {
                    if let Ok(turn) = &turn {
                        for line in turn.notices.iter().chain(&turn.rotated) {
                            crate::platform::notify("ccs", line);
                        }
                    }
                }
                if iced::futures::executor::block_on(tx.send(turn)).is_err() {
                    return;
                }
                // Sleep in short steps so a dropped stream is noticed soon.
                for _ in 0..300 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if tx.is_closed() {
                        return;
                    }
                }
            }
        });
        rx
    }
}

/// The gateway, up or down. Up, the listener and the desk that answers it
/// run on threads of this process; `pool` is what a limited request falls
/// over to. Changing the port or the pool takes it down and up again. The
/// line that comes back is what the window shows under the switch.
pub async fn gateway(on: bool, port: String, pool: Vec<String>) -> Result<String, Failure> {
    #[cfg(test)]
    {
        let _ = pool;
        return Ok(fixture::gateway(on, &port));
    }
    #[cfg(not(test))]
    {
        let mut running = GATEWAY.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(up) = running.take() {
            up.stop();
        }
        if !on {
            return Ok("off".to_string());
        }
        let port: u16 =
            port.trim().parse().map_err(|_| Failure::new(format!("{port:?} is not a port")))?;
        let env = env()?;
        let keys = ccs::cmd::gateway_keys(&env.ctx())?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| Failure::new(format!("listening on 127.0.0.1:{port}: {e}")))?;
        let (asks, inbox) = std::sync::mpsc::channel();
        let up = ccs::serve::listen(listener, keys, asks);
        let desk_env = Arc::clone(&env);
        std::thread::spawn(move || ccs::cmd::desk(&desk_env.ctx(), &pool, inbox));
        *running = Some(up);
        Ok(format!("serving http://127.0.0.1:{port} as the accounts in use"))
    }
}

/// The gateway that is up, if one is.
#[cfg(not(test))]
static GATEWAY: Mutex<Option<ccs::serve::Listening>> = Mutex::new(None);

/// Take the gateway down, for a quit that should not leave a port held.
pub async fn shutdown() {
    #[cfg(not(test))]
    if let Some(up) = GATEWAY.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
        up.stop();
    }
}

/// What the app remembers between launches.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub gateway_on: bool,
    #[serde(default = "default_port")]
    pub gateway_port: String,
    #[serde(default)]
    pub rotation_on: bool,
    #[serde(default)]
    pub pool: Vec<String>,
    #[serde(default = "yes")]
    pub notifications_on: bool,
    #[serde(default)]
    pub launch_at_login: bool,
}

fn default_port() -> String {
    "4141".to_string()
}

fn yes() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            gateway_on: false,
            gateway_port: default_port(),
            rotation_on: false,
            pool: Vec::new(),
            notifications_on: true,
            launch_at_login: false,
        }
    }
}

#[cfg(not(test))]
const PREFS_FILE: &str = "app.json";

/// Where the preferences live: beside the stash, which is where the app's
/// state belongs.
#[cfg(not(test))]
fn prefs_path() -> Result<std::path::PathBuf, Failure> {
    Ok(env()?.ctx().stash.root().join(PREFS_FILE))
}

/// Read the preferences, or the defaults. Missing and broken read the same:
/// a file this program cannot read is not one it should reason from.
pub fn read_prefs(path: &std::path::Path) -> Prefs {
    std::fs::read(path).ok().and_then(|raw| serde_json::from_slice(&raw).ok()).unwrap_or_default()
}

pub fn write_prefs(path: &std::path::Path, prefs: &Prefs) -> Result<(), Failure> {
    let body = serde_json::to_vec_pretty(prefs).map_err(|e| Failure::new(e.to_string()))?;
    ccs::fsx::write_atomic(path, &body, 0o600)?;
    Ok(())
}

/// The preferences as the program starts: each state field asks for its
/// own at initialization.
pub fn prefs() -> Prefs {
    #[cfg(test)]
    return Prefs::default();
    #[cfg(not(test))]
    prefs_path().map(|path| read_prefs(&path)).unwrap_or_default()
}

pub fn pref_gateway_on() -> bool {
    prefs().gateway_on
}

pub fn pref_gateway_port() -> String {
    prefs().gateway_port
}

pub fn pref_rotation_on() -> bool {
    prefs().rotation_on
}

pub fn pref_pool() -> Vec<String> {
    prefs().pool
}

pub fn pref_notifications_on() -> bool {
    prefs().notifications_on
}

pub fn pref_launch_at_login() -> bool {
    prefs().launch_at_login
}

/// Write the preferences down. Every toggle calls this; the answer is
/// whether it took, for the line under the switch to say.
pub fn save_prefs(
    gateway_on: bool,
    gateway_port: String,
    rotation_on: bool,
    pool: Vec<String>,
    notifications_on: bool,
    launch_at_login: bool,
) -> bool {
    let prefs =
        Prefs { gateway_on, gateway_port, rotation_on, pool, notifications_on, launch_at_login };
    #[cfg(test)]
    {
        let _ = prefs;
        true
    }
    #[cfg(not(test))]
    prefs_path().and_then(|path| write_prefs(&path, &prefs)).is_ok()
}

/// Start at login or stop; what comes back is what is now set.
pub async fn launch_at_login(on: bool) -> Result<bool, Failure> {
    #[cfg(test)]
    return Ok(on);
    #[cfg(not(test))]
    crate::platform::launch_at_login(on).map_err(Failure::from)
}

/// The accounts a first-class test starts from, as a state initializer can
/// ask for them; empty outside tests, where the real reading comes from
/// `load`.
pub fn fixture_accounts() -> Vec<Account> {
    #[cfg(test)]
    return fixture::reset();
    #[cfg(not(test))]
    Vec::new()
}

/// The stash a first-class test sees. Externs are real in those tests, so
/// the real thing is replaced here, under `cfg(test)`, by one that answers
/// the way the stash would: a switch marks one account of the provider
/// active and no other, and refuses a spent one unless forced.
#[cfg(test)]
pub mod fixture {
    use super::{Account, Failure, Limit};
    use std::sync::Mutex;

    static ACCOUNTS: Mutex<Vec<Account>> = Mutex::new(Vec::new());

    fn limit(column: &str, percent: f64, health: &str) -> Limit {
        Limit { column: column.into(), percent, resets_in: "3h04m".into(), health: health.into() }
    }

    fn account(provider: &str, slug: &str, active: bool, session: f64, spent: bool) -> Account {
        Account {
            provider: provider.into(),
            slug: slug.into(),
            email: format!("{slug}@example.com"),
            plan: if provider == "codex" { "codex pro".into() } else { "max20x".into() },
            active,
            spent,
            session_percent: session,
            polled: "2m ago".into(),
            note: String::new(),
            limits: vec![
                limit("session", session.max(0.0), if spent { "spent" } else { "ok" }),
                limit("weekly", 37.0, "ok"),
            ],
        }
    }

    /// Three Claude accounts, one spent, and one Codex account; `hong` in use.
    pub fn reset() -> Vec<Account> {
        let accounts = vec![
            account("claude", "agent", false, 6.0, false),
            account("codex", "codex-frost", true, -1.0, false),
            account("claude", "hong", true, 52.0, false),
            account("claude", "robin", false, 100.0, true),
        ];
        *ACCOUNTS.lock().expect("fixture") = accounts.clone();
        accounts
    }

    pub fn load() -> Result<Vec<Account>, Failure> {
        let held = ACCOUNTS.lock().expect("fixture").clone();
        Ok(if held.is_empty() { reset() } else { held })
    }

    /// One turn, then the end: the fixture's accounts with `agent` in use,
    /// as if the watcher had rotated onto it.
    pub fn watch() -> impl super::Stream<Item = Result<super::Poll, Failure>> + Send + 'static {
        let mut accounts = reset();
        for account in accounts.iter_mut().filter(|a| a.provider == "claude") {
            account.active = account.slug == "agent";
        }
        *ACCOUNTS.lock().expect("fixture") = accounts.clone();
        let turn = super::Poll {
            accounts,
            notices: vec!["session-high: hong@example.com has crossed 90%".into()],
            rotated: vec!["switched to agent@example.com".into()],
        };
        iced::futures::stream::once(async move { Ok(turn) })
    }

    pub fn gateway(on: bool, port: &str) -> String {
        match on {
            true => format!("serving http://127.0.0.1:{port} as the accounts in use"),
            false => "off".into(),
        }
    }

    pub fn switch(slug: &str, force: bool) -> Result<Vec<Account>, Failure> {
        if ACCOUNTS.lock().expect("fixture").is_empty() {
            reset();
        }
        let mut held = ACCOUNTS.lock().expect("fixture");
        let Some(target) = held.iter().find(|a| a.slug == slug).cloned() else {
            return Err(Failure::new(format!("no stashed account matches {slug:?}")));
        };
        if target.spent && !force {
            return Err(Failure::spent(&target.slug, &target.email));
        }
        for account in held.iter_mut().filter(|a| a.provider == target.provider) {
            account.active = account.slug == slug;
        }
        Ok(held.clone())
    }
}

/// The cache's rows in the view's terms, aged against `now`.
pub fn accounts_of(readings: &[Cached], now: Timestamp) -> Vec<Account> {
    readings
        .iter()
        .map(|reading| {
            let entry = &reading.entry;
            let limits: Vec<Limit> = entry
                .known()
                .iter()
                .map(|limit| Limit {
                    column: limit.column(),
                    percent: limit.percent,
                    resets_in: limit
                        .resets_at
                        .as_deref()
                        .and_then(render::until)
                        .unwrap_or_default(),
                    health: match limit.health() {
                        Health::Ok => "ok",
                        Health::Warn => "warn",
                        Health::Critical => "spent",
                    }
                    .into(),
                })
                .collect();
            Account {
                provider: entry.provider.to_string(),
                slug: entry.slug.clone(),
                email: entry.email.clone(),
                plan: entry.plan.clone(),
                active: entry.active,
                spent: !entry.exhausted().is_empty(),
                session_percent: entry
                    .known()
                    .iter()
                    .find(|l| l.kind == "session")
                    .map_or(-1.0, |l| l.percent),
                polled: reading.polled_at.as_deref().map(|at| ago(at, now)).unwrap_or_default(),
                note: entry.limits.as_ref().err().cloned().unwrap_or_default(),
                limits,
            }
        })
        .collect()
}

/// How long ago `rfc3339` was, in the CLI's units.
fn ago(rfc3339: &str, now: Timestamp) -> String {
    let Ok(then) = rfc3339.parse::<Timestamp>() else { return String::new() };
    let seconds = (now.as_second() - then.as_second()).max(0);
    format!("{} ago", render::compact(seconds.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs::model::{Limit as Reading, Provider};

    #[test]
    fn preferences_round_trip_and_missing_ones_are_the_defaults() {
        let dir = std::env::temp_dir().join(format!("ccs-app-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("app.json");

        assert_eq!(read_prefs(&path), Prefs::default());
        let prefs = Prefs { gateway_on: true, pool: vec!["a".into()], ..Prefs::default() };
        write_prefs(&path, &prefs).expect("writes");
        assert_eq!(read_prefs(&path), prefs);
        std::fs::write(&path, "not json").expect("write");
        assert_eq!(read_prefs(&path), Prefs::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_file_missing_a_field_still_reads() {
        let dir = std::env::temp_dir().join(format!("ccs-app-prefs-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("app.json");
        std::fs::write(&path, r#"{"gateway_on": true}"#).expect("write");
        let prefs = read_prefs(&path);
        assert!(prefs.gateway_on);
        assert_eq!(prefs.gateway_port, "4141");
        assert!(prefs.notifications_on);
        let _ = std::fs::remove_dir_all(&dir);
    }
    use ccs::render::Entry;

    fn cached(slug: &str, active: bool, limits: Result<Vec<Reading>, String>) -> Cached {
        Cached {
            entry: Entry {
                provider: Provider::Claude,
                slug: slug.into(),
                email: format!("{slug}@x.com"),
                plan: "max20x".into(),
                active,
                limits,
            },
            polled_at: Some("2026-09-07T00:00:00Z".into()),
        }
    }

    fn reading(kind: &str, percent: f64, resets_at: &str) -> Reading {
        Reading {
            kind: kind.into(),
            percent,
            severity: None,
            resets_at: Some(resets_at.into()),
            scope: None,
        }
    }

    #[test]
    fn a_cached_row_becomes_an_account_with_its_limits_named_and_judged() {
        let now: Timestamp = "2026-09-07T00:04:30Z".parse().expect("stamp");
        let rows = vec![cached(
            "h",
            true,
            Ok(vec![
                reading("session", 52.0, "2099-01-01T00:00:00Z"),
                reading("weekly_all", 100.0, "2099-01-01T00:00:00Z"),
            ]),
        )];

        let accounts = accounts_of(&rows, now);

        assert_eq!(accounts.len(), 1);
        let account = &accounts[0];
        assert_eq!(
            (account.provider.as_str(), account.slug.as_str(), account.active),
            ("claude", "h", true)
        );
        assert_eq!(account.session_percent, 52.0);
        assert!(account.spent);
        assert_eq!(account.polled, "4m ago");
        assert_eq!(account.limits[0].column, "session");
        assert_eq!(account.limits[0].health, "ok");
        assert_eq!(account.limits[1].column, "weekly");
        assert_eq!(account.limits[1].health, "spent");
        assert!(!account.limits[0].resets_in.is_empty());
    }

    #[test]
    fn an_account_without_a_reading_carries_the_reason_and_no_session() {
        let rows = vec![cached("n", false, Err("not polled yet".into()))];
        let account = &accounts_of(&rows, Timestamp::now())[0];
        assert_eq!(account.note, "not polled yet");
        assert_eq!(account.session_percent, -1.0);
        assert!(account.limits.is_empty());
        assert!(!account.spent);
    }
}
