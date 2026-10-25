//! Terminal setup/teardown helpers.
//!
//! Extracted from `tui/app.rs` in Phase 2.2.

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture, KeyEvent, KeyEventKind},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{Stdout, stdout};

/// `mouse_capture` routes wheel and click events to the app. Turning it off lets the terminal
/// select and copy text with the mouse, at the cost of wheel scrolling inside the TUI.
pub fn setup_terminal(mouse_capture: bool) -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<CrosstermBackend<Stdout>>> = (|| {
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        if mouse_capture {
            execute!(out, EnableMouseCapture)?;
        }
        execute!(out, Hide)?;
        execute!(out, Clear(ClearType::All))?;
        Ok(Terminal::new(CrosstermBackend::new(out))?)
    })();
    if res.is_err() {
        let _ = disable_raw_mode();
    }
    res
}

pub fn restore_terminal() {
    let mut out = stdout();
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableMouseCapture);
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Windows reports each key as a press and a release event. Only presses and repeats reach the
/// UI; handling both doubles every keystroke ("hhaalloo").
pub fn is_actionable_key(key: &KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEventState, KeyModifiers};

    fn key(kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn release_events_are_dropped_so_keystrokes_are_not_doubled() {
        assert!(is_actionable_key(&key(KeyEventKind::Press)));
        assert!(is_actionable_key(&key(KeyEventKind::Repeat)));
        assert!(!is_actionable_key(&key(KeyEventKind::Release)));
    }
}
