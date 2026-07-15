//! `wizard::term` byte writers (AC-12): the enter/leave helpers emit the
//! real crossterm alt-screen control sequences into an `impl Write` buffer.

use std::error::Error;

use observability::wizard::term::{enter_terminal, leave_terminal};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Whether `haystack` contains the byte subsequence `needle`.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
  haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn enter_terminal_writes_alt_screen_enter() -> TestResult {
  let mut buf: Vec<u8> = Vec::new();
  enter_terminal(&mut buf)?;
  assert!(!buf.is_empty(), "enter must write command bytes");
  assert!(
    contains_bytes(&buf, b"\x1b[?1049h"),
    "enter must emit the alt-screen-enter sequence, got {buf:?}"
  );
  Ok(())
}

#[test]
fn leave_terminal_writes_alt_screen_exit() -> TestResult {
  let mut buf: Vec<u8> = Vec::new();
  leave_terminal(&mut buf)?;
  assert!(!buf.is_empty(), "leave must write command bytes");
  assert!(
    contains_bytes(&buf, b"\x1b[?1049l"),
    "leave must emit the alt-screen-exit sequence, got {buf:?}"
  );
  Ok(())
}
