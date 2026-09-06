//! What changes between two looks at the accounts that a subscriber would
//! want to hear about. Pure: the loop that polls and the inbox that delivers
//! live in `cmd` and `notify`.

use std::collections::HashMap;

use jiff::Timestamp;

use crate::model::{Limit, Provider};
use crate::render::{Entry, until};

/// The notices a session can subscribe to. `switch` is raised by `ccs use`;
/// the rest by `ccs watch`.
pub const KINDS: [&str; 4] = ["switch", "session-high", "session-reset", "weekly-reset"];

/// What one look at one account remembered for the next look.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    pub session_pct: Option<f64>,
    /// Unix seconds. The API's fractional part drifts between polls, so the
    /// instant is kept only to the second.
    pub session_resets_at: Option<i64>,
    pub weekly_resets_at: Option<i64>,
}

pub type Snapshot = HashMap<String, Seen>;

/// A notice: which kind, and the line to deliver.
#[derive(Debug, PartialEq)]
pub struct Event {
    pub provider: Provider,
    pub kind: &'static str,
    pub text: String,
}

pub fn snapshot(entries: &[Entry]) -> Snapshot {
    entries
        .iter()
        .filter(|e| e.limits.is_ok())
        .map(|e| {
            let session = limit(e, "session");
            (
                e.slug.clone(),
                Seen {
                    session_pct: session.map(|l| l.percent),
                    session_resets_at: session.and_then(at),
                    weekly_resets_at: limit(e, "weekly_all").and_then(at),
                },
            )
        })
        .collect()
}

/// Everything worth saying about `now` given what `before` looked like.
/// `high` is the session percentage at which the active account is called out.
pub fn diff(before: &Snapshot, entries: &[Entry], high: f64) -> Vec<Event> {
    diff_at(before, entries, high, Timestamp::now().as_second())
}

fn diff_at(before: &Snapshot, entries: &[Entry], high: f64, now: i64) -> Vec<Event> {
    let mut events = Vec::new();
    for e in entries {
        let active_weekly = entries
            .iter()
            .find(|active| active.active && active.provider == e.provider)
            .and_then(|active| at(limit(active, "weekly_all")?));
        if e.limits.is_err() {
            continue;
        }
        let was = before.get(&e.slug);
        let session = limit(e, "session");
        let weekly = limit(e, "weekly_all");

        if e.active
            && let Some(s) = session
            && s.percent >= high
            && was.and_then(|w| w.session_pct).is_none_or(|p| p < high)
        {
            events.push(Event {
                provider: e.provider,
                kind: "session-high",
                text: format!(
                    "{} session at {:.0}%{}; weekly {}",
                    e.email,
                    s.percent,
                    resets(s),
                    weekly.map(standing).unwrap_or_default()
                ),
            });
        }

        if let Some(w) = was
            && came_back(w.session_resets_at, session.and_then(at), now)
        {
            let sooner = match (active_weekly, weekly.and_then(at)) {
                (Some(a), Some(mine)) if mine < a => ", sooner than the active account's",
                _ => "",
            };
            events.push(Event {
                provider: e.provider,
                kind: "session-reset",
                text: format!(
                    "{} session window reset; weekly {}{sooner}",
                    e.email,
                    weekly.map(standing).unwrap_or_default()
                ),
            });
        }

        if let Some(w) = was
            && came_back(w.weekly_resets_at, weekly.and_then(at), now)
        {
            events.push(Event {
                provider: e.provider,
                kind: "weekly-reset",
                text: format!("{} weekly window reset", e.email),
            });
        }
    }
    events
}

/// Which account in `pool` the active one should give way to, if any.
///
/// The active account keeps its place while it is in the pool with session
/// headroom below `high` and weekly headroom at all. Once it has none, the
/// pool member whose weekly window resets soonest and still has room takes
/// over, so quota about to be forfeited is burned first; ties go to the
/// emptier session. An active account outside the pool was chosen by hand and
/// is left alone, and so is one whose standing could not be read this poll: a
/// failed probe is not an empty account.
///
/// A switch is only made to an account `HYSTERESIS` points clear of the mark,
/// so a rolling window that has just dipped under it does not pull the active
/// account straight back.
#[cfg(test)]
fn rotate<'a>(entries: &'a [Entry], pool: &[String], high: f64) -> Option<&'a Entry> {
    rotate_within(entries, Provider::Claude, pool, high)
}

/// Every switch worth making: one per provider at most, each decided among
/// that provider's rows alone, since each has an account in use of its own.
pub fn rotations<'a>(entries: &'a [Entry], pool: &[String], high: f64) -> Vec<&'a Entry> {
    Provider::ALL.iter().filter_map(|p| rotate_within(entries, *p, pool, high)).collect()
}

fn rotate_within<'a>(
    entries: &'a [Entry],
    provider: Provider,
    pool: &[String],
    high: f64,
) -> Option<&'a Entry> {
    let active = entries.iter().find(|e| e.active && e.provider == provider)?;
    if active.limits.is_err() || !pool.contains(&active.slug) || has_room(active, high) {
        return None;
    }
    entries
        .iter()
        .filter(|e| {
            e.provider == provider
                && !e.active
                && pool.contains(&e.slug)
                && has_room(e, high - HYSTERESIS)
        })
        .min_by(|a, b| {
            let key = |e: &Entry| {
                (
                    limit(e, "weekly_all").and_then(at).unwrap_or(i64::MAX),
                    limit(e, "session").map(|l| l.percent).unwrap_or(0.0) as i64,
                )
            };
            key(a).cmp(&key(b))
        })
}

/// How far under the high mark an account must be before it is switched to.
const HYSTERESIS: f64 = 15.0;

fn has_room(e: &Entry, high: f64) -> bool {
    e.limits.is_ok()
        && limit(e, "session").is_none_or(|s| s.percent < high)
        && limit(e, "weekly_all").is_none_or(|w| !w.exhausted())
}

/// A window came back when the reset it was heading for has passed and the
/// limit no longer points at it.
fn came_back(then: Option<i64>, now_at: Option<i64>, now: i64) -> bool {
    matches!(then, Some(t) if t <= now && now_at != Some(t))
}

fn at(limit: &Limit) -> Option<i64> {
    limit.resets_at.as_deref()?.parse::<Timestamp>().ok().map(|t| t.as_second())
}

fn limit<'a>(entry: &'a Entry, kind: &str) -> Option<&'a Limit> {
    entry.known().iter().find(|l| l.kind == kind && l.scope.is_none())
}

fn resets(limit: &Limit) -> String {
    limit
        .resets_at
        .as_deref()
        .and_then(until)
        .map(|d| format!(", resets in {d}"))
        .unwrap_or_default()
}

fn standing(limit: &Limit) -> String {
    format!("{:.0}%{}", limit.percent, resets(limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        slug: &str,
        active: bool,
        session: (f64, Option<&str>),
        weekly: Option<&str>,
    ) -> Entry {
        let lim = |kind: &str, percent, resets_at: Option<&str>| Limit {
            kind: kind.into(),
            percent,
            severity: None,
            resets_at: resets_at.map(String::from),
            scope: None,
        };
        Entry {
            provider: Provider::Claude,
            slug: slug.into(),
            email: format!("{slug}@x"),
            plan: "max".into(),
            active,
            limits: Ok(vec![lim("session", session.0, session.1), lim("weekly_all", 10.0, weekly)]),
        }
    }

    fn codex_entry(slug: &str, active: bool, weekly: f64) -> Entry {
        let mut entry = entry(slug, active, (0.0, None), None);
        entry.provider = Provider::Codex;
        entry.limits = Ok(vec![Limit {
            kind: "weekly_all".into(),
            percent: weekly,
            severity: None,
            resets_at: None,
            scope: None,
        }]);
        entry
    }

    /// Both providers produce events; delivery uses the provider on each event.
    #[test]
    fn notices_name_the_provider_they_belong_to() {
        let before: Snapshot = [
            (
                "a".to_string(),
                Seen { session_pct: Some(10.0), session_resets_at: None, weekly_resets_at: None },
            ),
            (
                "gpt".to_string(),
                Seen { session_pct: Some(10.0), session_resets_at: None, weekly_resets_at: None },
            ),
        ]
        .into_iter()
        .collect();
        let mut gpt = codex_entry("gpt", true, 95.0);
        gpt.limits = Ok(vec![Limit {
            kind: "session".into(),
            percent: 95.0,
            severity: None,
            resets_at: None,
            scope: None,
        }]);
        let now = entry("a", true, (95.0, None), None);
        let events = diff_at(&before, &[gpt, now], 90.0, 0);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].provider, Provider::Codex);
        assert_eq!(events[1].provider, Provider::Claude);
        assert!(events[1].text.contains("a@x"), "{}", events[1].text);
    }

    /// Each provider rotates among its own rows: a spent Claude account
    /// hands over to a Claude account, never to a Codex one, and the other
    /// way round.
    #[test]
    fn rotation_keeps_to_each_providers_own_rows() {
        let rows = vec![
            entry("a", true, (99.0, None), Some(W1)),
            entry("b", false, (5.0, None), Some(W0)),
            codex_entry("gpt", true, 100.0),
            codex_entry("gpt2", false, 10.0),
        ];
        let pool = ["a", "b", "gpt", "gpt2"].map(String::from).to_vec();
        let moves: Vec<&str> =
            rotations(&rows, &pool, 90.0).iter().map(|e| e.slug.as_str()).collect();
        assert_eq!(moves, ["b", "gpt2"]);
    }

    const T1: &str = "2026-09-05T14:50:00Z";
    const W1: &str = "2026-09-06T00:00:00Z";
    const W0: &str = "2026-09-05T20:00:00Z";
    fn secs(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_second()
    }

    #[test]
    fn crossing_the_high_mark_fires_once() {
        let a = [entry("a", true, (85.0, Some(T1)), Some(W1))];
        let b = [entry("a", true, (92.0, Some(T1)), Some(W1))];
        assert!(diff(&snapshot(&a), &a, 90.0).is_empty());
        let events = diff(&snapshot(&a), &b, 90.0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "session-high");
        assert!(diff(&snapshot(&b), &b, 90.0).is_empty(), "already above: nothing new");
    }

    #[test]
    fn a_window_that_comes_back_is_a_reset_and_says_whose_weekly_is_sooner() {
        let before = [
            entry("a", true, (100.0, Some(T1)), Some(W1)),
            entry("b", false, (100.0, Some("2026-09-05T14:00:00Z")), Some(W0)),
        ];
        let after = [
            entry("a", true, (100.0, Some(T1)), Some(W1)),
            entry("b", false, (0.0, None), Some(W0)),
        ];
        let events = diff_at(&snapshot(&before), &after, 90.0, secs("2026-09-05T14:01:00Z"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "session-reset");
        assert!(events[0].text.contains("b@x"));
        assert!(events[0].text.ends_with("sooner than the active account's"));
    }

    #[test]
    fn a_reset_still_ahead_is_not_a_reset_however_the_fraction_drifts() {
        let a = [entry("a", true, (50.0, Some("2026-09-05T14:50:00.634Z")), Some(W1))];
        let b = [entry("a", true, (50.0, Some("2026-09-05T14:50:00.556Z")), Some(W1))];
        assert!(diff_at(&snapshot(&a), &b, 90.0, secs("2026-09-05T14:00:00Z")).is_empty());
        // Past the instant, a moved-on window is a reset even if the API still
        // reports a fresh one.
        let c = [entry("a", true, (1.0, Some("2026-09-05T19:50:00Z")), Some(W1))];
        let events = diff_at(&snapshot(&a), &c, 90.0, secs("2026-09-05T14:51:00Z"));
        assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), ["session-reset"]);
    }

    fn pool(slugs: &[&str]) -> Vec<String> {
        slugs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_active_account_with_headroom_keeps_its_place() {
        let e = [
            entry("a", true, (50.0, Some(T1)), Some(W1)),
            entry("b", false, (0.0, None), Some(W0)),
        ];
        assert!(rotate(&e, &pool(&["a", "b"]), 90.0).is_none());
    }

    #[test]
    fn a_high_session_gives_way_to_the_soonest_weekly_reset_with_room() {
        let e = [
            entry("a", true, (95.0, Some(T1)), Some(W1)),
            entry("b", false, (10.0, Some(T1)), Some("2026-09-07T00:00:00Z")),
            entry("c", false, (30.0, Some(T1)), Some(W0)),
            entry("d", false, (0.0, None), Some("2026-09-01T00:00:00Z")),
        ];
        // d resets soonest of all but is not in the pool; c beats b on weekly.
        assert_eq!(rotate(&e, &pool(&["a", "b", "c"]), 90.0).map(|e| e.slug.as_str()), Some("c"));
    }

    #[test]
    fn a_pool_member_without_room_is_skipped_and_ties_go_to_the_emptier_session() {
        let e = [
            entry("a", true, (95.0, Some(T1)), Some(W1)),
            entry("b", false, (92.0, Some(T1)), Some(W0)),
            entry("c", false, (30.0, Some(T1)), Some(W1)),
            entry("d", false, (10.0, Some(T1)), Some(W1)),
        ];
        assert_eq!(
            rotate(&e, &pool(&["a", "b", "c", "d"]), 90.0).map(|e| e.slug.as_str()),
            Some("d")
        );
    }

    #[test]
    fn an_account_just_under_the_mark_is_not_switched_to() {
        let e = [
            entry("a", true, (96.0, Some(T1)), Some(W1)),
            entry("b", false, (80.0, Some(T1)), Some(W0)),
            entry("c", false, (70.0, Some(T1)), Some(W1)),
        ];
        // b's weekly is sooner, but 80 is within HYSTERESIS of 90; c is clear.
        assert_eq!(rotate(&e, &pool(&["a", "b", "c"]), 90.0).map(|e| e.slug.as_str()), Some("c"));
    }

    #[test]
    fn an_active_account_whose_probe_failed_is_not_mistaken_for_an_empty_one() {
        let mut a = entry("a", true, (0.0, None), Some(W1));
        a.limits = Err("429".into());
        let e = [a, entry("b", false, (0.0, None), Some(W0))];
        assert!(rotate(&e, &pool(&["a", "b"]), 90.0).is_none());
    }

    #[test]
    fn nobody_with_room_means_nobody_to_switch_to() {
        let e = [
            entry("a", true, (95.0, Some(T1)), Some(W1)),
            entry("b", false, (100.0, Some(T1)), Some(W0)),
        ];
        assert!(rotate(&e, &pool(&["a", "b"]), 90.0).is_none());
    }

    #[test]
    fn an_active_account_outside_the_pool_is_left_alone() {
        let e = [
            entry("x", true, (99.0, Some(T1)), Some(W1)),
            entry("a", false, (0.0, None), Some(W0)),
        ];
        assert!(rotate(&e, &pool(&["a"]), 90.0).is_none());
    }

    #[test]
    fn first_look_has_nothing_to_compare_against_except_the_high_mark() {
        let now = [entry("a", true, (95.0, Some(T1)), Some(W1))];
        let events = diff(&Snapshot::new(), &now, 90.0);
        assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), ["session-high"]);
    }
}
