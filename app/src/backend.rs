//! The Rust side of the app: what the Ice program calls across its typed
//! boundary. The stash, the network and the platform live behind it; the
//! program reads what it is handed.

use std::sync::{Arc, Mutex, OnceLock};

use ccs::cmd::{self, Cached};
use ccs::env::Env;
use ccs::model::Health;
use ccs::render;
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

/// What went wrong, for the window to say.
#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        let message = error.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": ");
        Self { message }
    }
}

/// Everything a command borrows, opened once for the life of the process
/// and shared with every thread that asks.
static ENV: OnceLock<Result<Arc<Env>, String>> = OnceLock::new();

/// The stash is one thing however many threads reach for it: the watcher's,
/// the gateway desk's, and the handlers'. Everything that reads it as a whole
/// or writes it goes through here.
static STASH: Mutex<()> = Mutex::new(());

fn env() -> Result<Arc<Env>, Failure> {
    let opened = ENV.get_or_init(|| Env::open().map(Arc::new).map_err(|e| Failure::from(e).message));
    opened.clone().map_err(|message| Failure { message })
}

/// The accounts as the watcher last wrote them down. Never polls: opening
/// the window twenty times costs the limits nothing.
pub async fn load() -> Result<Vec<Account>, Failure> {
    let env = env()?;
    let _stash = STASH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let readings = cmd::readings(&env.ctx())?;
    Ok(accounts_of(&readings, Timestamp::now()))
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
                    resets_in: limit.resets_at.as_deref().and_then(render::until).unwrap_or_default(),
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
            Ok(vec![reading("session", 52.0, "2099-01-01T00:00:00Z"), reading("weekly_all", 100.0, "2099-01-01T00:00:00Z")]),
        )];

        let accounts = accounts_of(&rows, now);

        assert_eq!(accounts.len(), 1);
        let account = &accounts[0];
        assert_eq!((account.provider.as_str(), account.slug.as_str(), account.active), ("claude", "h", true));
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
