//! The `pure` side of the boundary: text the view and the tray compose from
//! the accounts. Deterministic, effect-free, and tested here rather than
//! through a window.

use crate::backend::{Account, PoolEntry};

/// What the menu bar shows: the active Claude account's session, since that
/// is the window that runs out within a working day.
pub fn bar_label(accounts: &[Account]) -> String {
    accounts
        .iter()
        .find(|a| a.active && a.provider == "claude" && a.session_percent >= 0.0)
        .map(|a| percent_label(a.session_percent))
        .unwrap_or_else(|| "–".to_string())
}

/// The `index`th account of `provider`, in listing order.
fn nth<'a>(accounts: &'a [Account], index: i64, provider: &str) -> Option<&'a Account> {
    let index = usize::try_from(index).ok()?;
    accounts.iter().filter(|a| a.provider == provider).nth(index)
}

/// Whether a tray slot has an account to show.
pub fn has(accounts: &[Account], index: i64, provider: String) -> bool {
    nth(accounts, index, &provider).is_some()
}

/// The slug behind a tray slot, for the handler that switches to it.
pub fn slot(accounts: &[Account], index: i64, provider: String) -> String {
    nth(accounts, index, &provider).map(|a| a.slug.clone()).unwrap_or_default()
}

/// A tray slot's whole text: the mark, who, the plan, and every limit — a
/// native menu row is one line of text, so the readout and the command are
/// the same row.
pub fn row(accounts: &[Account], index: i64, provider: String) -> String {
    let Some(account) = nth(accounts, index, &provider) else { return String::new() };
    let mut parts =
        vec![format!("{} {}", mark(account.active), account.email), account.plan.clone()];
    if account.limits.is_empty() {
        if !account.note.is_empty() {
            parts.push(account.note.clone());
        }
    } else {
        for limit in &account.limits {
            parts.push(format!("{} {}", limit.column, percent_label(limit.percent)));
        }
    }
    parts.join(" · ")
}

/// Whether the account `slug` is the one in use, for tests to ask.
pub fn active_of(accounts: &[Account], slug: String) -> bool {
    accounts.iter().any(|a| a.slug == slug && a.active)
}

/// What the confirmation asks before switching into a spent account.
pub fn confirm_question(accounts: &[Account], slug: &str) -> String {
    match accounts.iter().find(|a| a.slug == slug) {
        Some(account) => {
            format!("{} has nothing left on one of its limits. Switch anyway?", account.email)
        }
        None => String::new(),
    }
}

/// Whether `slug` is in the rotation pool.
pub fn in_pool(pool: &[String], slug: String) -> bool {
    pool.contains(&slug)
}

/// Whether `port` names a port a listener can take.
pub fn is_port(port: &str) -> bool {
    port.trim().parse::<u16>().is_ok_and(|p| p > 0)
}

/// What the gateway line says as a port is applied: a refusal when it is
/// not one, "off" when the gateway is, and that it is coming up otherwise.
pub fn port_line(port: &str, on: bool) -> String {
    match (is_port(port), on) {
        (false, _) => format!("{port:?} is not a port; 1 to 65535"),
        (true, false) => "off".to_string(),
        (true, true) => format!("starting on {}", port.trim()),
    }
}

/// Whether the pool rows show `slug` ticked, for tests to ask.
pub fn ticked_in(rows: &[PoolEntry], slug: String) -> bool {
    rows.iter().any(|r| r.slug == slug && r.ticked)
}

/// The pool with `slug` added or taken out.
pub fn toggled(pool: &[String], slug: String, on: bool) -> Vec<String> {
    let mut next: Vec<String> = pool.iter().filter(|s| **s != slug).cloned().collect();
    if on {
        next.push(slug);
    }
    next
}

/// Every account with whether it is in the pool.
pub fn pool_rows(accounts: &[Account], pool: &[String]) -> Vec<PoolEntry> {
    accounts
        .iter()
        .map(|a| PoolEntry {
            slug: a.slug.clone(),
            email: a.email.clone(),
            ticked: pool.contains(&a.slug),
        })
        .collect()
}

/// The pool the daemons are handed: the one ticked, only while rotation is on.
pub fn pool_for(rotation_on: bool, pool: &[String]) -> Vec<String> {
    match rotation_on {
        true => pool.to_vec(),
        false => Vec::new(),
    }
}

/// The tray's gateway row: what it is doing, and what pressing it does.
pub fn gateway_row(on: bool, port: String) -> String {
    match on {
        true => format!("Gateway on :{port} — turn off"),
        false => format!("Gateway off — serve on :{port}"),
    }
}

/// The tray's rotation row.
pub fn rotation_row(on: bool) -> String {
    match on {
        true => "Rotating automatically — stop".to_string(),
        false => "Not rotating — rotate automatically".to_string(),
    }
}

/// When the watcher last looked: the newest reading's clock.
pub fn polled_line(accounts: &[Account]) -> String {
    let newest = accounts
        .iter()
        .filter(|a| !a.polled_at.is_empty())
        .max_by(|a, b| a.polled_at.cmp(&b.polled_at));
    match newest {
        Some(account) => format!("polled {}", account.polled),
        None => "not polled yet".to_string(),
    }
}

/// What the watcher's last turn came to, in a line under its switch.
pub fn watcher_said(notices: &[String], rotated: &[String]) -> String {
    let mut lines: Vec<&str> = notices.iter().map(String::as_str).collect();
    lines.extend(rotated.iter().map(String::as_str));
    match lines.is_empty() {
        true => "polled; nothing to report".to_string(),
        false => lines.join(" · "),
    }
}

/// A percentage as the table prints it; a dash for a window not reported.
pub fn percent_label(percent: f64) -> String {
    match percent < 0.0 {
        true => "–".to_string(),
        false => format!("{}%", percent.round() as i64),
    }
}

/// The active marker, as one character.
pub fn mark(active: bool) -> String {
    match active {
        true => "●",
        false => "○",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Account, Limit};

    fn limit(column: &str, percent: f64) -> Limit {
        Limit { column: column.into(), percent, resets_in: "3h04m".into(), health: "ok".into() }
    }

    fn account(provider: &str, slug: &str, active: bool, session: f64) -> Account {
        Account {
            provider: provider.into(),
            slug: slug.into(),
            email: format!("{slug}@x.com"),
            plan: if provider == "codex" { "codex pro".into() } else { "max20x".into() },
            active,
            spent: false,
            session_percent: session,
            polled_at: "2026-09-07T00:00:00Z".into(),
            polled: "09:00".into(),
            note: String::new(),
            limits: vec![limit("session", session), limit("weekly", 37.0), limit("Fable", 62.0)],
        }
    }

    #[test]
    fn the_bar_shows_the_active_claude_accounts_session() {
        let accounts = vec![
            account("codex", "g", true, -1.0),
            account("claude", "a", false, 5.0),
            account("claude", "h", true, 52.4),
        ];
        assert_eq!(bar_label(&accounts), "52%");
    }

    #[test]
    fn the_bar_shows_a_dash_when_nothing_is_known() {
        assert_eq!(bar_label(&[]), "–");
        assert_eq!(bar_label(&[account("codex", "g", true, -1.0)]), "–");
    }

    #[test]
    fn a_slot_names_the_nth_account_of_its_provider() {
        let accounts = vec![
            account("claude", "a", false, 5.0),
            account("codex", "g", true, -1.0),
            account("claude", "h", true, 52.0),
        ];
        assert!(has(&accounts, 0, "claude".into()));
        assert!(has(&accounts, 1, "claude".into()));
        assert!(!has(&accounts, 2, "claude".into()));
        assert!(has(&accounts, 0, "codex".into()));
        assert!(!has(&accounts, 1, "codex".into()));
        assert_eq!(slot(&accounts, 1, "claude".into()), "h");
        assert_eq!(slot(&accounts, 0, "codex".into()), "g");
        assert_eq!(slot(&accounts, 5, "codex".into()), "");
    }

    #[test]
    fn a_row_reads_as_one_line_with_the_mark_and_every_limit() {
        let accounts = vec![account("claude", "h", true, 52.0), account("claude", "a", false, 5.0)];
        assert_eq!(
            row(&accounts, 0, "claude".into()),
            "● h@x.com · max20x · session 52% · weekly 37% · Fable 62%"
        );
        assert_eq!(
            row(&accounts, 1, "claude".into()),
            "○ a@x.com · max20x · session 5% · weekly 37% · Fable 62%"
        );
        assert_eq!(row(&accounts, 3, "claude".into()), "");
    }

    #[test]
    fn a_row_says_when_an_account_has_no_reading() {
        let mut unread = account("claude", "n", false, -1.0);
        unread.limits.clear();
        unread.note = "not polled yet".into();
        assert_eq!(row(&[unread], 0, "claude".into()), "○ n@x.com · max20x · not polled yet");
    }

    #[test]
    fn the_question_names_the_account_and_nobody_when_there_is_none() {
        let accounts = vec![account("claude", "r", false, 100.0)];
        assert_eq!(
            confirm_question(&accounts, "r"),
            "r@x.com has nothing left on one of its limits. Switch anyway?"
        );
        assert_eq!(confirm_question(&accounts, "x"), "");
        assert!(!active_of(&accounts, "r".into()));
    }

    #[test]
    fn the_pool_is_ticked_and_unticked_by_slug() {
        let pool = vec!["a".to_string()];
        assert!(in_pool(&pool, "a".into()));
        assert!(!in_pool(&pool, "b".into()));
        assert_eq!(toggled(&pool, "b".into(), true), ["a", "b"]);
        assert_eq!(toggled(&pool, "a".into(), false), Vec::<String>::new());
        assert_eq!(toggled(&pool, "a".into(), true), ["a"]);
        assert_eq!(pool_for(false, &pool), Vec::<String>::new());
        assert_eq!(pool_for(true, &pool), ["a"]);
        let rows = pool_rows(
            &[account("claude", "a", true, 5.0), account("claude", "b", false, 5.0)],
            &pool,
        );
        assert_eq!(
            rows.iter().map(|r| (r.slug.as_str(), r.ticked)).collect::<Vec<_>>(),
            [("a", true), ("b", false)]
        );
    }

    #[test]
    fn the_daemon_rows_say_what_pressing_them_does() {
        assert_eq!(gateway_row(true, "4141".into()), "Gateway on :4141 — turn off");
        assert_eq!(gateway_row(false, "4141".into()), "Gateway off — serve on :4141");
        assert_eq!(rotation_row(false), "Not rotating — rotate automatically");
        assert_eq!(polled_line(&[account("claude", "a", true, 5.0)]), "polled 09:00");
        assert_eq!(polled_line(&[]), "not polled yet");
        assert!(is_port("4141") && !is_port("0") && !is_port("70000") && !is_port("x"));
    }

    #[test]
    fn the_watcher_line_is_what_the_turn_did_or_that_it_did_nothing() {
        assert_eq!(watcher_said(&[], &[]), "polled; nothing to report");
        assert_eq!(
            watcher_said(&["session-high: a".into()], &["switched to b".into()]),
            "session-high: a · switched to b"
        );
    }

    #[test]
    fn a_percent_is_whole_and_a_mark_is_a_dot() {
        assert_eq!(percent_label(52.4), "52%");
        assert_eq!(percent_label(-1.0), "–");
        assert_eq!(mark(true), "●");
        assert_eq!(mark(false), "○");
    }
}
