//! Thin, testable terminal command writers over any `impl Write`. Only the
//! alt-screen + cursor bytes live here so they can be asserted against a
//! `Vec<u8>`; raw-mode enable/disable are global side effects owned by the
//! bin's terminal guard, not this module.

use std::io::Write;

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};

/// Switch `w` to the alternate screen and hide the cursor.
///
/// # Errors
///
/// Propagates the underlying `io::Error` if writing the command bytes fails.
pub fn enter_terminal(w: &mut impl Write) -> std::io::Result<()> {
  execute!(w, EnterAlternateScreen, Hide)
}

/// Restore `w` from the alternate screen and show the cursor.
///
/// # Errors
///
/// Propagates the underlying `io::Error` if writing the command bytes fails.
pub fn leave_terminal(w: &mut impl Write) -> std::io::Result<()> {
  execute!(w, LeaveAlternateScreen, Show)
}
