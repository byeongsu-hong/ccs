//! Interactive account picker.

use std::io::{self, Write};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue};

use crate::render::{self, Style, Table};

/// How the picker was closed.
pub enum Outcome {
    /// Switch to the account at this index.
    Switch(usize),
    /// Re-probe usage and come back, holding this cursor position.
    Refresh(usize),
    Quit,
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

pub fn run(table: &Table, start: usize) -> Result<Outcome> {
    if table.entries().is_empty() {
        return Ok(Outcome::Quit);
    }
    let _screen = Screen::enter()?;
    let style = Style::colored();
    let mut at = start.min(table.len() - 1);
    loop {
        draw(table, at, style)?;
        let Event::Key(key) = event::read().context("reading key")? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match decide(key, at, table.len()) {
            Step::Move(next) => at = next,
            Step::Done(outcome) => return Ok(outcome),
            Step::Ignore => {}
        }
    }
}

enum Step {
    Move(usize),
    Done(Outcome),
    Ignore,
}

/// Key handling as a pure decision, leaving the loop above about drawing.
fn decide(key: KeyEvent, at: usize, len: usize) -> Step {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => Step::Done(Outcome::Quit),
        KeyCode::Char('q') | KeyCode::Esc => Step::Done(Outcome::Quit),
        KeyCode::Enter => Step::Done(Outcome::Switch(at)),
        KeyCode::Char('r') => Step::Done(Outcome::Refresh(at)),
        KeyCode::Up | KeyCode::Char('k') => Step::Move(at.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => Step::Move((at + 1).min(len - 1)),
        KeyCode::Home => Step::Move(0),
        KeyCode::End => Step::Move(len - 1),
        _ => Step::Ignore,
    }
}

fn draw(table: &Table, at: usize, style: Style) -> Result<()> {
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
    write!(out, "\r\n{}\r\n", style.dim("  up/down select   enter switch   r refresh   q quit"))?;
    out.flush()?;
    Ok(())
}
