//! The `pure` side of the boundary: text the view and the tray compose from
//! the accounts. Deterministic, effect-free, and tested here rather than
//! through a window.

use crate::backend::Account;

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
    let mut parts = vec![format!("{} {}", mark(account.active), account.email), account.plan.clone()];
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
            polled: "2m ago".into(),
            note: String::new(),
            limits: vec![limit("session", session), limit("weekly", 37.0), limit("Fable", 62.0)],
        }
    }

    #[test]
    fn the_bar_shows_the_active_claude_accounts_session() {
        let accounts =
            vec![account("codex", "g", true, -1.0), account("claude", "a", false, 5.0), account("claude", "h", true, 52.4)];
        assert_eq!(bar_label(&accounts), "52%");
    }

    #[test]
    fn the_bar_shows_a_dash_when_nothing_is_known() {
        assert_eq!(bar_label(&[]), "–");
        assert_eq!(bar_label(&[account("codex", "g", true, -1.0)]), "–");
    }

    #[test]
    fn a_slot_names_the_nth_account_of_its_provider() {
        let accounts =
            vec![account("claude", "a", false, 5.0), account("codex", "g", true, -1.0), account("claude", "h", true, 52.0)];
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
        assert_eq!(row(&accounts, 0, "claude".into()), "● h@x.com · max20x · session 52% · weekly 37% · Fable 62%");
        assert_eq!(row(&accounts, 1, "claude".into()), "○ a@x.com · max20x · session 5% · weekly 37% · Fable 62%");
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
    fn a_percent_is_whole_and_a_mark_is_a_dot() {
        assert_eq!(percent_label(52.4), "52%");
        assert_eq!(percent_label(-1.0), "–");
        assert_eq!(mark(true), "●");
        assert_eq!(mark(false), "○");
    }
}
