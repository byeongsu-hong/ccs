//! Rendering the usage table: bars, colour, and the dynamic column set.
//!
//! Columns are derived from whatever limits the API reports rather than fixed
//! in code, so a newly scoped model shows up on its own.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::io::IsTerminal;

use jiff::Timestamp;

use crate::model::{Health, Limit};

/// Width of a usage bar, in cells.
const BAR: usize = 4;

/// Rendered width of a reset countdown. The longest a limit can be away is a
/// week, so `23h59m` is the widest this gets.
const COUNTDOWN: usize = 6;

/// Rendered width of a populated cell: the bar, a right-aligned percentage, and
/// the countdown to that limit coming back.
const CELL: usize = BAR + 1 + 4 + 1 + COUNTDOWN;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

/// What the table needs to know about one account. Deliberately free of stash
/// and API types so the table stays a pure formatter.
pub struct Entry {
    pub slug: String,
    pub email: String,
    pub plan: String,
    pub active: bool,
    /// The account's limits, or why they could not be read. One field rather
    /// than two, so "has limits" and "failed" cannot both be true at once.
    pub limits: Result<Vec<Limit>, String>,
}

impl Entry {
    /// The limits that were readable; empty when the probe failed.
    pub fn known(&self) -> &[Limit] {
        self.limits.as_deref().unwrap_or(&[])
    }

    /// The worst standing across this account's limits, which is what decides
    /// whether it is still worth switching to.
    pub fn health(&self) -> Health {
        self.known().iter().map(Limit::health).fold(Health::Ok, |worst, h| match (worst, h) {
            (Health::Critical, _) | (_, Health::Critical) => Health::Critical,
            (Health::Warn, _) | (_, Health::Warn) => Health::Warn,
            _ => Health::Ok,
        })
    }

    /// Limits with nothing left on them, named for a human.
    pub fn exhausted(&self) -> Vec<String> {
        self.known().iter().filter(|l| l.exhausted()).map(Limit::column).collect()
    }
}

#[derive(Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    /// Colour when stdout is a terminal and the environment has not asked
    /// otherwise.
    pub fn detect() -> Self {
        let suppressed = std::env::var_os("NO_COLOR").is_some();
        Self { color: !suppressed && std::io::stdout().is_terminal() }
    }

    pub fn colored() -> Self {
        Self { color: true }
    }

    fn paint(&self, text: &str, code: &str) -> String {
        if !self.color || code.is_empty() {
            return text.to_string();
        }
        format!("{code}{text}{RESET}")
    }

    pub fn health(&self, text: &str, health: Health) -> String {
        self.paint(
            text,
            match health {
                Health::Ok => "\x1b[32m",
                Health::Warn => "\x1b[33m",
                Health::Critical => "\x1b[31m",
            },
        )
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(text, DIM)
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint(text, BOLD)
    }
}

pub struct Table {
    columns: Vec<String>,
    entries: Vec<Entry>,
    email_width: usize,
    plan_width: usize,
    style: Style,
}

impl Table {
    pub fn build(entries: Vec<Entry>, style: Style) -> Self {
        let columns = columns_of(&entries);
        let email_width =
            entries.iter().map(|e| e.email.len()).max().unwrap_or(0).max("ACCOUNT".len());
        let plan_width = entries.iter().map(|e| e.plan.len()).max().unwrap_or(0).max("PLAN".len());
        Self { columns, entries, email_width, plan_width, style }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn header(&self) -> String {
        let cells = self
            .columns
            .iter()
            .map(|c| format!("{:<width$}", c.to_uppercase(), width = CELL))
            .collect::<Vec<_>>()
            .join("  ");
        self.style.dim(&format!(
            "   {:<ew$}  {:<pw$}  {}",
            "ACCOUNT",
            "PLAN",
            cells,
            ew = self.email_width,
            pw = self.plan_width
        ))
    }

    /// One account's line, without any selection cursor: callers own that.
    pub fn row(&self, index: usize) -> String {
        let Some(entry) = self.entries.get(index) else { return String::new() };

        let head = format!(
            "{:>2} {:<ew$}  {:<pw$}",
            index + 1,
            entry.email,
            entry.plan,
            ew = self.email_width,
            pw = self.plan_width
        );
        let head = match entry.health() {
            Health::Critical => self.style.dim(&head),
            _ => head,
        };

        if let Err(error) = &entry.limits {
            return format!("{head}  {}", self.style.health(error, Health::Critical));
        }

        let cells = self
            .columns
            .iter()
            .map(|column| self.cell(entry, column))
            .collect::<Vec<_>>()
            .join("  ");
        let suffix = if entry.active { self.style.bold("  <- active") } else { String::new() };
        format!("{head}  {cells}{suffix}")
    }

    fn cell(&self, entry: &Entry, column: &str) -> String {
        let Some(limit) = entry.known().iter().find(|l| l.column() == column) else {
            return self.style.dim(&format!("{:<width$}", "—", width = CELL));
        };
        let reset = limit.resets_at.as_deref().and_then(until_compact).unwrap_or_default();
        let text = format!(
            "{} {:>3.0}% {reset:<COUNTDOWN$}",
            bar(limit.percent),
            limit.percent.clamp(0.0, 100.0)
        );
        self.style.health(&format!("{text:<CELL$}"), limit.health())
    }
}

/// The union of limit columns across every account, ordered so the two limits
/// that always exist lead and per-model weeklies follow by name.
fn columns_of(entries: &[Entry]) -> Vec<String> {
    let unique: BTreeSet<String> =
        entries.iter().flat_map(|e| e.known().iter().map(Limit::column)).collect();
    let mut columns: Vec<String> = unique.into_iter().collect();
    columns.sort_by(|a, b| match rank(a).cmp(&rank(b)) {
        Ordering::Equal => a.cmp(b),
        other => other,
    });
    columns
}

fn rank(column: &str) -> u8 {
    match column {
        "session" => 0,
        "weekly" => 1,
        _ => 2,
    }
}

/// A usage bar. Any non-zero usage fills at least one cell, so a barely-touched
/// limit still reads as touched.
fn bar(percent: f64) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    let filled = match clamped {
        p if p <= 0.0 => 0,
        p => ((p / 100.0) * BAR as f64).ceil() as usize,
    }
    .min(BAR);
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR - filled))
}

/// Time until an RFC 3339 instant, phrased for a glance.
pub fn until(rfc3339: &str) -> Option<String> {
    seconds_until(rfc3339).map(human)
}

/// The same countdown narrowed to fit a table cell.
fn until_compact(rfc3339: &str) -> Option<String> {
    seconds_until(rfc3339).map(compact)
}

fn seconds_until(rfc3339: &str) -> Option<i64> {
    let target: Timestamp = rfc3339.parse().ok()?;
    Some(target.as_second() - Timestamp::now().as_second())
}

/// A duration with the spaces squeezed out, for somewhere a column of them has
/// to line up.
fn compact(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_string();
    }
    let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, m) => format!("{m}m"),
        (h, m) if h < 24 => format!("{h}h{m:02}m"),
        (h, _) => format!("{}d{}h", h / 24, h % 24),
    }
}

/// The question to put before a switch, naming what is spent when something is.
/// Shared so the picker and the command line ask it the same way.
pub fn switch_question(entry: &Entry) -> String {
    let spent = entry.exhausted();
    match spent.is_empty() {
        true => format!("Switch to {}?", entry.email),
        false => format!("{} has no {} left. Switch anyway?", entry.email, spent.join(", ")),
    }
}

fn human(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_string();
    }
    let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, m) => format!("{m}m"),
        (h, m) if h < 24 => format!("{h}h {m:02}m"),
        (h, _) => format!("{}d {}h", h / 24, h % 24),
    }
}

/// Per-limit detail for one account: where each limit stands and when it comes
/// back. Shared by `ccs status` and the picker's footer.
pub fn detail(entry: &Entry, style: Style) -> Vec<String> {
    entry
        .known()
        .iter()
        .map(|limit| {
            let resets = limit
                .resets_at
                .as_deref()
                .and_then(until)
                .map(|d| format!("resets in {d}"))
                .unwrap_or_default();
            let note = if limit.exhausted() { "   spent" } else { "" };
            let body = format!(
                "  {:<16} {} {:>3.0}%   {resets}{note}",
                limit.column(),
                bar(limit.percent),
                limit.percent.clamp(0.0, 100.0),
            );
            style.health(&body, limit.health())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limit;

    fn plain() -> Style {
        Style { color: false }
    }

    fn entry(email: &str, limits: Vec<Limit>) -> Entry {
        Entry {
            slug: email.replace('@', "_at_"),
            email: email.into(),
            plan: "max20x".into(),
            active: false,
            limits: Ok(limits),
        }
    }

    #[test]
    fn a_bar_is_empty_only_at_genuine_zero() {
        assert_eq!(bar(0.0), "░░░░");
        assert_eq!(bar(0.4), "█░░░");
    }

    #[test]
    fn a_bar_fills_proportionally_and_clamps_past_full() {
        assert_eq!(bar(50.0), "██░░");
        assert_eq!(bar(100.0), "████");
        assert_eq!(bar(220.0), "████");
    }

    #[test]
    fn columns_lead_with_the_two_families_that_always_exist() {
        let entries = vec![entry(
            "a@x.com",
            vec![
                limit!("weekly_scoped", 10.0, model = "Opus"),
                limit!("weekly_all", 53.0),
                limit!("session", 3.0),
                limit!("weekly_scoped", 100.0, model = "Fable"),
            ],
        )];
        assert_eq!(columns_of(&entries), ["session", "weekly", "Fable", "Opus"]);
    }

    #[test]
    fn columns_are_the_union_across_accounts() {
        let entries = vec![
            entry("a@x.com", vec![limit!("weekly_scoped", 1.0, model = "Fable")]),
            entry("b@x.com", vec![limit!("weekly_scoped", 2.0, model = "Opus")]),
        ];
        assert_eq!(columns_of(&entries), ["Fable", "Opus"]);
    }

    #[test]
    fn an_account_is_as_healthy_as_its_worst_limit() {
        let spent = entry(
            "a@x.com",
            vec![limit!("session", 3.0), limit!("weekly_scoped", 100.0, model = "Fable")],
        );
        assert_eq!(spent.health(), Health::Critical);
        assert_eq!(spent.exhausted(), ["Fable"]);
    }

    #[test]
    fn an_unreadable_account_reports_no_limits_rather_than_pretending() {
        let broken = Entry {
            slug: "a".into(),
            email: "a@x.com".into(),
            plan: "?".into(),
            active: false,
            limits: Err("token rejected".into()),
        };
        assert!(broken.known().is_empty());
        assert!(broken.exhausted().is_empty());
    }

    #[test]
    fn a_row_shows_the_failure_instead_of_empty_bars() {
        let broken = Entry {
            slug: "a".into(),
            email: "a@x.com".into(),
            plan: "?".into(),
            active: false,
            limits: Err("token rejected".into()),
        };
        let table = Table::build(vec![broken], plain());
        assert!(table.row(0).contains("token rejected"));
    }

    #[test]
    fn a_row_marks_the_active_account_and_dashes_limits_it_lacks() {
        let mut active = entry("a@x.com", vec![limit!("session", 3.0)]);
        active.active = true;
        let other = entry("b@x.com", vec![limit!("weekly_scoped", 4.0, model = "Fable")]);
        let table = Table::build(vec![active, other], plain());

        let first = table.row(0);
        assert!(first.contains("<- active"), "{first}");
        assert!(
            first.contains("—"),
            "row should dash the Fable column it has no limit for: {first}"
        );
        assert!(!table.row(1).contains("<- active"));
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(human(-5), "now");
        assert_eq!(human(45), "45s");
        assert_eq!(human(9 * 60), "9m");
        assert_eq!(human(4 * 3600 + 12 * 60), "4h 12m");
        assert_eq!(human(50 * 3600), "2d 2h");
    }

    #[test]
    fn a_reset_already_past_reads_as_now() {
        assert_eq!(until("2020-01-01T00:00:00+00:00").as_deref(), Some("now"));
    }

    #[test]
    fn compact_durations_squeeze_out_the_spaces_to_fit_a_column() {
        assert_eq!(compact(-5), "now");
        assert_eq!(compact(45), "45s");
        assert_eq!(compact(9 * 60), "9m");
        assert_eq!(compact(4 * 3600 + 12 * 60), "4h12m");
        assert_eq!(compact(50 * 3600), "2d2h");
    }

    #[test]
    fn a_compact_duration_never_outgrows_its_column() {
        let widest = [0, 59, 60, 3599, 3600, 23 * 3600 + 59 * 60, 6 * 86400 + 23 * 3600];
        for seconds in widest {
            let rendered = compact(seconds);
            assert!(rendered.chars().count() <= COUNTDOWN, "{seconds}s renders as {rendered}");
        }
    }

    #[test]
    fn a_cell_carries_the_countdown_beside_the_percentage() {
        let past = "2020-01-01T00:00:00+00:00";
        let entries = vec![entry("a@x.com", vec![limit!("session", 50.0, resets = past)])];
        let row = Table::build(entries, plain()).row(0);
        assert!(row.contains("50%"), "{row}");
        assert!(row.contains("now"), "{row}");
    }

    #[test]
    fn the_switch_question_names_what_is_spent() {
        let fine = entry("a@x.com", vec![limit!("session", 3.0)]);
        assert_eq!(switch_question(&fine), "Switch to a@x.com?");

        let spent = entry("a@x.com", vec![limit!("weekly_scoped", 100.0, model = "Fable")]);
        assert!(switch_question(&spent).contains("no Fable left"), "{}", switch_question(&spent));
    }

    #[test]
    fn an_unparseable_reset_is_dropped_rather_than_guessed() {
        assert_eq!(until("not a timestamp"), None);
    }
}
