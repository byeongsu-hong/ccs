//! Interactive account picker.
//!
//! The picker owns the screen for its whole lifetime: refreshing happens
//! through an injected closure rather than by exiting and being re-entered, so
//! the list stays up while usage is re-polled.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue};

use crate::model::Health;
use crate::render::{self, Style, Table, Verb};

/// How the picker was closed.
pub enum Outcome {
    /// Act on the account with this slug. Carrying the slug rather than a row
    /// index keeps the answer meaningful after a refresh has rebuilt the table
    /// underneath it.
    Chose(String),
    Quit,
}

/// What the footer is saying, and whether keys mean what they usually mean.
enum Mode {
    Browsing,
    /// Waiting on a yes or no for the account in this row.
    Confirming(usize),
    /// A transient line: work in progress, or a refresh that did not land.
    /// Cosmetic only — every key still does its usual job.
    Note(String),
}

/// Restores the terminal however the picker exits, panic included.
struct Screen;

impl Screen {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode().context("entering raw mode")?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)
            .context("entering alternate screen")?;
        Ok(Self)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// How often the list re-polls on its own.
const AUTO_REFRESH: Duration = Duration::from_secs(60);

/// How long after a keypress an unattended poll holds off. Polling blocks for
/// as long as the network takes, so it waits for a lull rather than freezing
/// the list under someone mid-navigation.
const IDLE_GRACE: Duration = Duration::from_secs(2);

/// How long to wait on a key before looking at the clock again.
const TICK: Duration = Duration::from_millis(200);

/// Show the accounts and let one be chosen. Usage is polled through `refresh`,
/// with the screen already up — the first load, every minute after that, and
/// any time `r` is pressed — so the list is never taken away to fetch.
///
/// `verb` is what choosing will do, which the confirmation and the key list
/// both have to say plainly: a switch moves every session, a launch moves none.
pub fn run(refresh: impl Fn() -> Result<Table>, verb: Verb) -> Result<Outcome> {
    let _screen = Screen::enter()?;
    let style = Style::colored();

    note(style, "polling accounts…")?;
    let mut table = refresh()?;
    if table.entries().is_empty() {
        return Ok(Outcome::Quit);
    }

    let mut at = 0;
    let mut mode = Mode::Browsing;
    let mut polled = Instant::now();
    let mut attempted = Instant::now();
    let mut last_key = Instant::now();
    let mut painted = String::new();

    loop {
        // Repaint only on a real change, so the ticking age in the footer costs
        // one frame a second and nothing else costs any.
        let current = frame(&table, at, &mode, style, polled, verb);
        if current != painted {
            paint(&current)?;
            painted = current;
        }

        let mut poll_now = false;
        if event::poll(TICK).context("waiting for a key")? {
            let Event::Key(key) = event::read().context("reading key")? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            last_key = Instant::now();

            // A confirmation is modal: nothing else is listening until it is answered.
            if let Mode::Confirming(target) = mode {
                match answer(key) {
                    Answer::Yes => {
                        let Some(entry) = table.entries().get(target) else {
                            return Ok(Outcome::Quit);
                        };
                        return Ok(Outcome::Chose(entry.slug.clone()));
                    }
                    Answer::No => mode = Mode::Browsing,
                    Answer::Ignore => {}
                }
                continue;
            }

            match decide(key, at, table.len()) {
                Step::Move(next) => {
                    at = next;
                    mode = Mode::Browsing;
                }
                Step::Confirm => mode = Mode::Confirming(at),
                Step::Refresh => poll_now = true,
                Step::Quit => return Ok(Outcome::Quit),
                Step::Ignore => mode = Mode::Browsing,
            }
        } else if due(&mode, attempted.elapsed(), last_key.elapsed()) {
            poll_now = true;
        }

        if poll_now {
            mode = Mode::Note("refreshing…".to_string());
            let pending = frame(&table, at, &mode, style, polled, verb);
            paint(&pending)?;
            painted = pending;
            mode = repoll(&refresh, &mut table, &mut at, &mut polled, &mut attempted);
        }
    }
}

/// Whether an unattended poll is due: never while a question is on screen,
/// never inside the grace period after a keypress, and not before the interval
/// has run out.
fn due(mode: &Mode, since_attempt: Duration, since_key: Duration) -> bool {
    !matches!(mode, Mode::Confirming(_)) && since_attempt >= AUTO_REFRESH && since_key >= IDLE_GRACE
}

/// Re-poll, keeping the last good reading when it fails.
///
/// `attempted` moves either way so a failing endpoint is retried on the usual
/// interval rather than on every tick; `polled` only moves on success, because
/// it is what the footer reports as the age of what is on screen.
fn repoll<F: Fn() -> Result<Table>>(
    refresh: &F,
    table: &mut Table,
    at: &mut usize,
    polled: &mut Instant,
    attempted: &mut Instant,
) -> Mode {
    *attempted = Instant::now();
    match refresh() {
        Ok(next) => {
            *at = (*at).min(next.len().saturating_sub(1));
            *table = next;
            *polled = Instant::now();
            Mode::Browsing
        }
        Err(e) => Mode::Note(format!("refresh failed: {e}")),
    }
}

enum Step {
    Move(usize),
    Confirm,
    Refresh,
    Quit,
    Ignore,
}

enum Answer {
    Yes,
    No,
    Ignore,
}

/// Key handling as a pure decision, leaving the loop above about drawing.
fn decide(key: KeyEvent, at: usize, len: usize) -> Step {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => Step::Quit,
        KeyCode::Char('q') | KeyCode::Esc => Step::Quit,
        KeyCode::Enter => Step::Confirm,
        KeyCode::Char('r') => Step::Refresh,
        KeyCode::Up | KeyCode::Char('k') => Step::Move(at.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => Step::Move((at + 1).min(len - 1)),
        KeyCode::Home => Step::Move(0),
        KeyCode::End => Step::Move(len - 1),
        _ => Step::Ignore,
    }
}

/// Answering a confirmation. Enter counts as yes because it is the key that
/// raised the question; anything unrecognised is ignored rather than guessed,
/// so a stray keypress cannot move an account.
fn answer(key: KeyEvent) -> Answer {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => Answer::No,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Answer::Yes,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') | KeyCode::Esc => Answer::No,
        _ => Answer::Ignore,
    }
}

/// A bare message on an otherwise empty screen, for before there is a table to
/// put under it.
fn note(style: Style, text: &str) -> Result<()> {
    paint(&format!("\n  {}", style.dim(text)))
}

/// The whole screen as text, so the loop can tell whether anything moved before
/// spending a repaint on it.
fn frame(
    table: &Table,
    at: usize,
    mode: &Mode,
    style: Style,
    polled: Instant,
    verb: Verb,
) -> String {
    let mut lines = vec![format!("  {}", table.header())];
    for index in 0..table.len() {
        let marker = if index == at { "> " } else { "  " };
        lines.push(format!("{marker}{}", table.row(index)));
    }
    if let Some(entry) = table.entries().get(at) {
        lines.push(String::new());
        lines.extend(render::detail(entry, style));
    }
    lines.push(String::new());
    lines.push(footer(table, mode, style, polled, verb));
    lines.join("\n")
}

/// Clears line by line rather than clearing the screen up front, so a repaint
/// never shows an empty frame on the way through.
fn paint(frame: &str) -> Result<()> {
    let mut out = io::stdout().lock();
    queue!(out, cursor::MoveTo(0, 0))?;
    for line in frame.split('\n') {
        write!(out, "{line}")?;
        queue!(out, terminal::Clear(ClearType::UntilNewLine))?;
        write!(out, "\r\n")?;
    }
    queue!(out, terminal::Clear(ClearType::FromCursorDown))?;
    out.flush()?;
    Ok(())
}

fn footer(table: &Table, mode: &Mode, style: Style, polled: Instant, verb: Verb) -> String {
    match mode {
        Mode::Browsing => style.dim(&format!(
            "  up/down select   enter {}   r refresh   q quit      updated {}",
            verb.word(),
            age(polled.elapsed())
        )),
        Mode::Note(text) => style.bold(&format!("  {text}")),
        Mode::Confirming(target) => {
            let Some(entry) = table.entries().get(*target) else { return String::new() };
            let question = format!("  {} [y/n]", render::question(entry, verb));
            match entry.exhausted().is_empty() {
                true => style.bold(&question),
                false => style.health(&question, Health::Critical),
            }
        }
    }
}

/// How old what is on screen is, in the fewest words that say it.
fn age(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds < 2 {
        true => "just now".to_string(),
        false => format!("{} ago", render::compact(seconds as i64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_asks_rather_than_switching_outright() {
        assert!(matches!(decide(press(KeyCode::Enter), 0, 3), Step::Confirm));
    }

    #[test]
    fn movement_stays_inside_the_list() {
        assert!(matches!(decide(press(KeyCode::Up), 0, 3), Step::Move(0)));
        assert!(matches!(decide(press(KeyCode::Down), 2, 3), Step::Move(2)));
        assert!(matches!(decide(press(KeyCode::Down), 0, 3), Step::Move(1)));
        assert!(matches!(decide(press(KeyCode::End), 0, 3), Step::Move(2)));
    }

    #[test]
    fn quitting_answers_to_more_than_one_key() {
        assert!(matches!(decide(press(KeyCode::Char('q')), 0, 3), Step::Quit));
        assert!(matches!(decide(press(KeyCode::Esc), 0, 3), Step::Quit));
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(decide(interrupt, 0, 3), Step::Quit));
    }

    #[test]
    fn a_confirmation_takes_yes_or_the_key_that_raised_it() {
        assert!(matches!(answer(press(KeyCode::Char('y'))), Answer::Yes));
        assert!(matches!(answer(press(KeyCode::Char('Y'))), Answer::Yes));
        assert!(matches!(answer(press(KeyCode::Enter)), Answer::Yes));
    }

    #[test]
    fn a_confirmation_takes_no_and_every_way_of_backing_out() {
        assert!(matches!(answer(press(KeyCode::Char('n'))), Answer::No));
        assert!(matches!(answer(press(KeyCode::Esc)), Answer::No));
        assert!(matches!(answer(press(KeyCode::Char('q'))), Answer::No));
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(answer(interrupt), Answer::No));
    }

    #[test]
    fn an_unattended_poll_waits_for_the_interval() {
        assert!(!due(&Mode::Browsing, Duration::from_secs(30), Duration::from_secs(60)));
        assert!(due(&Mode::Browsing, AUTO_REFRESH, Duration::from_secs(60)));
    }

    #[test]
    fn an_unattended_poll_never_interrupts_a_question() {
        assert!(!due(&Mode::Confirming(0), AUTO_REFRESH, Duration::from_secs(60)));
    }

    #[test]
    fn an_unattended_poll_holds_off_right_after_a_keypress() {
        assert!(!due(&Mode::Browsing, AUTO_REFRESH, Duration::from_millis(200)));
        assert!(due(&Mode::Browsing, AUTO_REFRESH, IDLE_GRACE));
    }

    #[test]
    fn a_failed_poll_does_not_stop_the_next_one() {
        let noted = Mode::Note("refresh failed: offline".to_string());
        assert!(due(&noted, AUTO_REFRESH, Duration::from_secs(60)));
    }

    #[test]
    fn the_age_label_reads_plainly() {
        assert_eq!(age(Duration::from_secs(0)), "just now");
        assert_eq!(age(Duration::from_secs(1)), "just now");
        assert_eq!(age(Duration::from_secs(45)), "45s ago");
        assert_eq!(age(Duration::from_secs(90)), "1m ago");
    }

    #[test]
    fn a_stray_key_never_answers_a_confirmation() {
        for code in [KeyCode::Char('j'), KeyCode::Char('r'), KeyCode::Down, KeyCode::Char(' ')] {
            assert!(matches!(answer(press(code)), Answer::Ignore), "{code:?} should be ignored");
        }
    }
}
