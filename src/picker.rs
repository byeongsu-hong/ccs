//! Interactive account picker.
//!
//! The picker owns the screen for its whole lifetime: refreshing happens
//! through an injected closure rather than by exiting and being re-entered, so
//! the list stays up while usage is re-polled.

use std::io::{self, Write};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue};

use crate::model::Health;
use crate::render::{self, Style, Table};

/// How the picker was closed.
pub enum Outcome {
    /// Switch to the account with this slug. Carrying the slug rather than a
    /// row index keeps the answer meaningful after a refresh has rebuilt the
    /// table underneath it.
    Switch(String),
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

/// Show the accounts and let one be chosen. Usage is polled through `refresh`,
/// with the screen already up — including the first poll, so the wait is
/// visible instead of being a blank terminal.
pub fn run(refresh: impl Fn() -> Result<Table>) -> Result<Outcome> {
    let _screen = Screen::enter()?;
    let style = Style::colored();

    note(style, "polling accounts…")?;
    let mut table = refresh()?;
    if table.entries().is_empty() {
        return Ok(Outcome::Quit);
    }
    let mut at = 0;
    let mut mode = Mode::Browsing;

    loop {
        draw(&table, at, &mode, style)?;
        let Event::Key(key) = event::read().context("reading key")? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // A confirmation is modal: nothing else is listening until it is answered.
        if let Mode::Confirming(target) = mode {
            match answer(key) {
                Answer::Yes => {
                    let Some(entry) = table.entries().get(target) else {
                        return Ok(Outcome::Quit);
                    };
                    return Ok(Outcome::Switch(entry.slug.clone()));
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
            Step::Refresh => {
                mode = Mode::Note("refreshing…".to_string());
                draw(&table, at, &mode, style)?;
                mode = match refresh() {
                    Ok(next) => {
                        at = at.min(next.len().saturating_sub(1));
                        table = next;
                        Mode::Browsing
                    }
                    // The list still holds the last good reading, so say what
                    // went wrong and leave it standing.
                    Err(e) => Mode::Note(format!("refresh failed: {e}")),
                };
            }
            Step::Quit => return Ok(Outcome::Quit),
            Step::Ignore => mode = Mode::Browsing,
        }
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
    let mut out = io::stdout();
    queue!(out, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;
    write!(out, "\r\n  {}\r\n", style.dim(text))?;
    out.flush()?;
    Ok(())
}

fn draw(table: &Table, at: usize, mode: &Mode, style: Style) -> Result<()> {
    let mut out = io::stdout();
    queue!(out, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;
    write!(out, "  {}\r\n", table.header())?;
    for index in 0..table.len() {
        let marker = if index == at { "> " } else { "  " };
        write!(out, "{marker}{}\r\n", table.row(index))?;
    }
    if let Some(entry) = table.entries().get(at) {
        write!(out, "\r\n")?;
        for line in render::detail(entry, style) {
            write!(out, "{line}\r\n")?;
        }
    }
    write!(out, "\r\n{}\r\n", footer(table, mode, style))?;
    out.flush()?;
    Ok(())
}

fn footer(table: &Table, mode: &Mode, style: Style) -> String {
    match mode {
        Mode::Browsing => style.dim("  up/down select   enter switch   r refresh   q quit"),
        Mode::Note(text) => style.bold(&format!("  {text}")),
        Mode::Confirming(target) => {
            let Some(entry) = table.entries().get(*target) else { return String::new() };
            let question = format!("  {} [y/n]", render::switch_question(entry));
            match entry.exhausted().is_empty() {
                true => style.bold(&question),
                false => style.health(&question, Health::Critical),
            }
        }
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
    fn a_stray_key_never_answers_a_confirmation() {
        for code in [KeyCode::Char('j'), KeyCode::Char('r'), KeyCode::Down, KeyCode::Char(' ')] {
            assert!(matches!(answer(press(code)), Answer::Ignore), "{code:?} should be ignored");
        }
    }
}
