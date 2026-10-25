//! Inline `/dev/tty` prompts (#204): the prompt logic, rail rendering
//! and key handling are ours; raw mode comes from the safe `crossterm`
//! crate. `/dev/tty` is opened once and is the only prompt I/O, so a
//! piped stdin (`curl | bash`) is never consumed. A reader thread pumps
//! tty bytes over a channel; keys are parsed in this thread (a lone ESC
//! is told from an escape sequence by a 50 ms wait for the next byte),
//! which is also how the unit tests drive prompts with byte streams.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use super::plan::SetupError;

/// One parsed key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
  /// Arrow up.
  Up,
  /// Arrow down.
  Down,
  /// Space (toggle).
  Space,
  /// Enter (confirm).
  Enter,
  /// Escape alone (cancel this prompt; setup exits).
  Esc,
  /// Ctrl+C (cancel).
  CtrlC,
  /// Backspace.
  Backspace,
  /// A printable character.
  Char(char),
}

/// How a prompt ended without a value.
#[derive(Debug)]
pub(crate) enum PromptEnd {
  /// Ctrl+C: the whole run cancels (exit 130).
  Cancelled,
  /// Esc: cancels this prompt; setup exits (the menu stages do not
  /// implement per-step back-navigation).
  Back,
  /// The terminal could not be read or written.
  Io(String),
  /// The terminal closed (EOF on `/dev/tty`): nobody is there to
  /// answer, so setup stops cleanly instead of failing.
  Closed,
}

impl From<PromptEnd> for SetupError {
  fn from(end: PromptEnd) -> Self {
    match end {
      PromptEnd::Cancelled => SetupError::cancelled(),
      PromptEnd::Back => SetupError::usage("cancelled at the prompt (Esc)"),
      PromptEnd::Io(message) => SetupError::runtime(message),
      PromptEnd::Closed => SetupError::closed_terminal(),
    }
  }
}

/// A stream of keys: the tty reader in production, a byte Vec in tests.
pub(crate) trait KeySource {
  /// The next key, blocking until one arrives.
  fn next_key(&mut self) -> Result<Key, PromptEnd>;
}

/// Parse the key at the front of `bytes` (at least one byte), returning
/// the key and the bytes it consumed. An incomplete escape sequence at
/// the end of a chunk parses as [`Key::Esc`] (the tty reader refills
/// before calling this, so only a real lone ESC reaches that case).
#[cfg(test)]
pub(crate) fn parse_key(bytes: &[u8]) -> Option<(Key, usize)> {
  let first = *bytes.first()?;
  let key = match first {
    0x03 => (Key::CtrlC, 1),
    b'\r' | b'\n' => (Key::Enter, 1),
    b' ' => (Key::Space, 1),
    0x7f | 0x08 => (Key::Backspace, 1),
    byte if byte >= 0x80 => {
      // A multi-byte UTF-8 character typed at the terminal.
      let text = std::str::from_utf8(bytes).unwrap_or("");
      match text.chars().next() {
        Some(c) => (Key::Char(c), c.len_utf8()),
        None => (Key::Char(byte as char), 1),
      }
    },
    0x1b => match (bytes.get(1), bytes.get(2)) {
      (Some(b'['), Some(b'A')) => (Key::Up, 3),
      (Some(b'['), Some(b'B')) => (Key::Down, 3),
      (Some(b'['), Some(_)) => (Key::Char('?'), 3),
      _ => (Key::Esc, 1),
    },
    byte => (Key::Char(byte as char), 1),
  };
  Some(key)
}

/// Keys parsed from a fixed byte string (tests).
#[cfg(test)]
pub(crate) struct ByteKeys {
  pending: VecDeque<u8>,
}

#[cfg(test)]
impl ByteKeys {
  /// Keys for `bytes`, parsed one at a time.
  pub(crate) fn new(bytes: &[u8]) -> Self {
    Self {
      pending: bytes.iter().copied().collect(),
    }
  }
}

#[cfg(test)]
impl KeySource for ByteKeys {
  fn next_key(&mut self) -> Result<Key, PromptEnd> {
    let front: Vec<u8> = self.pending.iter().copied().collect();
    let Some((key, used)) = parse_key(&front) else {
      return Err(PromptEnd::Io("prompt input ended".to_owned()));
    };
    for _ in 0..used {
      self.pending.pop_front();
    }
    Ok(key)
  }
}

/// Whether raw mode is currently held (the panic hook's restore check).
static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Restore the terminal from a panic or signal: disable raw mode once.
pub(crate) fn restore_terminal() {
  if RAW_ACTIVE.swap(false, Ordering::SeqCst) {
    let _ = crossterm::terminal::disable_raw_mode();
  }
}

/// Raw-mode guard: restores on drop, and chains a panic hook that
/// restores before the previous hook runs (`panic = "abort"` never
/// unwinds, so the hook is the only restore path on panic).
/// The chained panic-hook type.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

struct RawGuard {
  previous_hook: Option<PanicHook>,
}

impl RawGuard {
  fn new() -> Result<Self, PromptEnd> {
    crossterm::terminal::enable_raw_mode()
      .map_err(|e| PromptEnd::Io(format!("could not enter raw terminal mode: {e}")))?;
    RAW_ACTIVE.store(true, Ordering::SeqCst);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
      restore_terminal();
      // The previous hook was taken by the guard; the default hook is
      // re-created here only for the message, never installed.
      eprintln!("{info}");
    }));
    Ok(Self {
      previous_hook: Some(previous),
    })
  }
}

impl Drop for RawGuard {
  fn drop(&mut self) {
    restore_terminal();
    if let Some(previous) = self.previous_hook.take() {
      std::panic::set_hook(previous);
    }
  }
}

/// The live prompt set over `/dev/tty`.
pub(crate) struct Prompts {
  keys: TtyKeys,
  out: std::fs::File,
}

/// Keys read from the `/dev/tty` byte channel.
struct TtyKeys {
  rx: Receiver<u8>,
  pending: VecDeque<u8>,
}

impl TtyKeys {
  fn byte(&mut self) -> Result<u8, PromptEnd> {
    if let Some(byte) = self.pending.pop_front() {
      return Ok(byte);
    }
    self.rx.recv().map_err(|_closed| PromptEnd::Closed)
  }

  fn byte_within(&mut self, wait: Duration) -> Option<u8> {
    if let Some(byte) = self.pending.pop_front() {
      return Some(byte);
    }
    self.rx.recv_timeout(wait).ok()
  }
}

impl KeySource for TtyKeys {
  fn next_key(&mut self) -> Result<Key, PromptEnd> {
    let first = self.byte()?;
    let key = match first {
      0x03 => Key::CtrlC,
      b'\r' | b'\n' => Key::Enter,
      b' ' => Key::Space,
      0x7f | 0x08 => Key::Backspace,
      0x1b => match self.byte_within(Duration::from_millis(50)) {
        Some(b'[') => match self.byte_within(Duration::from_millis(50)) {
          Some(b'A') => Key::Up,
          Some(b'B') => Key::Down,
          Some(_) => Key::Char('?'),
          None => Key::Esc,
        },
        Some(other) => {
          self.pending.push_back(other);
          Key::Esc
        },
        None => Key::Esc,
      },
      byte if byte >= 0x80 => {
        let width = match byte {
          0xC0..=0xDF => 2,
          0xE0..=0xEF => 3,
          _ => 4,
        };
        let mut buf = vec![byte];
        for _ in 1..width {
          match self.byte() {
            Ok(next) => buf.push(next),
            Err(_) => break,
          }
        }
        std::str::from_utf8(&buf)
          .ok()
          .and_then(|text| text.chars().next())
          .map_or(Key::Char(byte as char), Key::Char)
      },
      byte => Key::Char(byte as char),
    };
    Ok(key)
  }
}

impl Prompts {
  /// Open `/dev/tty` (read+write) and start the byte reader thread.
  /// Raw mode is entered per prompt by `RawGuard`, never held across
  /// apply, so Ctrl+C keeps working outside prompts. Fails when no
  /// terminal is available — callers treat that as unattended, never
  /// as an echoed-secret fallback.
  pub(crate) fn open() -> Result<Self, PromptEnd> {
    use std::io::Read as _;
    let mut reader = std::fs::OpenOptions::new()
      .read(true)
      .write(true)
      .open("/dev/tty")
      .map_err(|e| PromptEnd::Io(format!("no terminal available (/dev/tty: {e})")))?;
    let out = reader
      .try_clone()
      .map_err(|e| PromptEnd::Io(format!("could not open the terminal for writing: {e}")))?;
    let (tx, rx) = channel::<u8>();
    std::thread::spawn(move || {
      let mut buf = [0_u8; 64];
      loop {
        match reader.read(&mut buf) {
          Ok(0) | Err(_) => return,
          Ok(n) => {
            for byte in buf.iter().take(n) {
              if tx.send(*byte).is_err() {
                return;
              }
            }
          },
        }
      }
    });
    Ok(Self {
      keys: TtyKeys {
        rx,
        pending: VecDeque::new(),
      },
      out,
    })
  }

  /// One-of-many selection (`↑/↓`, enter, esc cancels, ctrl+c cancels).
  pub(crate) fn select(
    &mut self,
    title: &str,
    options: &[String],
    default: usize,
  ) -> Result<usize, PromptEnd> {
    let _guard = RawGuard::new()?;
    run_select(&mut self.keys, &mut self.out, title, options, default)
  }

  /// Many-of-many selection (space toggles, `a` all, enter confirms).
  pub(crate) fn multi_select(
    &mut self,
    title: &str,
    options: &[String],
    preselected: &[bool],
  ) -> Result<Vec<usize>, PromptEnd> {
    let _guard = RawGuard::new()?;
    run_multi_select(&mut self.keys, &mut self.out, title, options, preselected)
  }

  /// A one-line text answer (enter confirms, esc cancels).
  pub(crate) fn input(&mut self, title: &str, default: &str) -> Result<String, PromptEnd> {
    let _guard = RawGuard::new()?;
    run_input(&mut self.keys, &mut self.out, title, default)
  }

  /// A secret line: typed bytes never reach the terminal.
  pub(crate) fn secret(&mut self, title: &str) -> Result<String, PromptEnd> {
    let _guard = RawGuard::new()?;
    run_secret(&mut self.keys, &mut self.out, title)
  }

  /// A yes/no answer (arrows or `y`/`n`, enter confirms).
  pub(crate) fn confirm(&mut self, title: &str, default: bool) -> Result<bool, PromptEnd> {
    let _guard = RawGuard::new()?;
    run_confirm(&mut self.keys, &mut self.out, title, default)
  }
}

fn write(out: &mut impl std::io::Write, text: &str) -> Result<(), PromptEnd> {
  out
    .write_all(text.as_bytes())
    .and_then(|()| out.flush())
    .map_err(|e| PromptEnd::Io(format!("could not write to the terminal: {e}")))
}

/// One-of-many selection over any key source (the tty in production, a
/// byte stream in tests).
pub(crate) fn run_select(
  keys: &mut impl KeySource,
  out: &mut impl std::io::Write,
  title: &str,
  options: &[String],
  default: usize,
) -> Result<usize, PromptEnd> {
  let mut index = default.min(options.len().saturating_sub(1));
  render_select(out, title, options, index)?;
  loop {
    match keys.next_key()? {
      Key::Up => {
        index = index
          .checked_sub(1)
          .unwrap_or(options.len().saturating_sub(1));
        render_select(out, title, options, index)?;
      },
      Key::Down => {
        index = (index + 1) % options.len().max(1);
        render_select(out, title, options, index)?;
      },
      Key::Enter => {
        write(out, "\r\n")?;
        return Ok(index);
      },
      Key::Esc => return Err(PromptEnd::Back),
      Key::CtrlC => return Err(PromptEnd::Cancelled),
      Key::Char(digit) if digit.is_ascii_digit() => {
        if let Some(chosen) = digit.to_digit(10).map(|d| (d as usize).saturating_sub(1))
          && chosen < options.len()
        {
          write(out, "\r\n")?;
          return Ok(chosen);
        }
      },
      Key::Space | Key::Backspace | Key::Char(_) => {},
    }
  }
}

fn render_select(
  out: &mut impl std::io::Write,
  title: &str,
  options: &[String],
  index: usize,
) -> Result<(), PromptEnd> {
  let mut text = format!("\r\x1b[K◆ {title}\r\n");
  for (i, option) in options.iter().enumerate() {
    let marker = if i == index { "›" } else { " " };
    let _ = write!(text, "\r\x1b[K│ {marker} {option}\r\n");
  }
  let _ = write!(text, "\x1b[{}A", options.len() + 1);
  write(out, &text)
}

/// Many-of-many selection over any key source.
pub(crate) fn run_multi_select(
  keys: &mut impl KeySource,
  out: &mut impl std::io::Write,
  title: &str,
  options: &[String],
  preselected: &[bool],
) -> Result<Vec<usize>, PromptEnd> {
  let mut chosen: Vec<bool> = preselected.to_vec();
  chosen.resize(options.len(), false);
  let mut index = 0_usize;
  render_multi(out, title, options, &chosen, index)?;
  loop {
    match keys.next_key()? {
      Key::Up => {
        index = index
          .checked_sub(1)
          .unwrap_or(options.len().saturating_sub(1));
        render_multi(out, title, options, &chosen, index)?;
      },
      Key::Down => {
        index = (index + 1) % options.len().max(1);
        render_multi(out, title, options, &chosen, index)?;
      },
      Key::Space => {
        if let Some(slot) = chosen.get_mut(index) {
          *slot = !*slot;
        }
        render_multi(out, title, options, &chosen, index)?;
      },
      Key::Char('a') => {
        let all = chosen.iter().all(|c| *c);
        chosen.fill(!all);
        render_multi(out, title, options, &chosen, index)?;
      },
      Key::Enter => {
        write(out, "\r\n")?;
        return Ok(
          chosen
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.then_some(i))
            .collect(),
        );
      },
      Key::Esc => return Err(PromptEnd::Back),
      Key::CtrlC => return Err(PromptEnd::Cancelled),
      Key::Backspace | Key::Char(_) => {},
    }
  }
}

fn render_multi(
  out: &mut impl std::io::Write,
  title: &str,
  options: &[String],
  chosen: &[bool],
  index: usize,
) -> Result<(), PromptEnd> {
  let mut text = format!("\r\x1b[K◆ {title} (space toggles, a = all, enter confirms)\r\n");
  for (i, option) in options.iter().enumerate() {
    let marker = if i == index { "›" } else { " " };
    let box_ = if chosen.get(i).copied().unwrap_or(false) {
      "[x]"
    } else {
      "[ ]"
    };
    let _ = write!(text, "\r\x1b[K│ {marker} {box_} {option}\r\n");
  }
  let _ = write!(text, "\x1b[{}A", options.len() + 1);
  write(out, &text)
}

/// A one-line text answer over any key source.
pub(crate) fn run_input(
  keys: &mut impl KeySource,
  out: &mut impl std::io::Write,
  title: &str,
  default: &str,
) -> Result<String, PromptEnd> {
  let mut value = String::new();
  write(out, &format!("◆ {title} [{default}]: "))?;
  loop {
    match keys.next_key()? {
      Key::Enter => {
        write(out, "\r\n")?;
        return Ok(if value.is_empty() {
          default.to_owned()
        } else {
          value
        });
      },
      Key::Space => {
        value.push(' ');
        write(out, " ")?;
      },
      Key::Backspace => {
        if value.pop().is_some() {
          write(out, "\x08 \x08")?;
        }
      },
      Key::Char(c) if !c.is_control() => {
        value.push(c);
        write(out, &c.to_string())?;
      },
      Key::Esc => return Err(PromptEnd::Back),
      Key::CtrlC => return Err(PromptEnd::Cancelled),
      Key::Up | Key::Down | Key::Char(_) => {},
    }
  }
}

/// A secret line over any key source: typed bytes never reach the writer.
pub(crate) fn run_secret(
  keys: &mut impl KeySource,
  out: &mut impl std::io::Write,
  title: &str,
) -> Result<String, PromptEnd> {
  let mut value = String::new();
  write(out, &format!("◆ {title}: "))?;
  loop {
    match keys.next_key()? {
      Key::Enter => {
        write(out, "••••\r\n")?;
        return Ok(value);
      },
      Key::Space => value.push(' '),
      Key::Backspace => {
        value.pop();
      },
      Key::Char(c) if !c.is_control() => value.push(c),
      Key::Esc => return Err(PromptEnd::Back),
      Key::CtrlC => return Err(PromptEnd::Cancelled),
      Key::Up | Key::Down | Key::Char(_) => {},
    }
  }
}

/// A yes/no answer over any key source.
pub(crate) fn run_confirm(
  keys: &mut impl KeySource,
  out: &mut impl std::io::Write,
  title: &str,
  default: bool,
) -> Result<bool, PromptEnd> {
  let mut value = default;
  loop {
    write(
      out,
      &format!(
        "\r\x1b[K◆ {title} [{}]",
        if value { "Yes / no" } else { "yes / No" }
      ),
    )?;
    match keys.next_key()? {
      Key::Up | Key::Down | Key::Space => value = !value,
      Key::Char('y' | 'Y') => {
        write(out, "\r\n")?;
        return Ok(true);
      },
      Key::Char('n' | 'N') => {
        write(out, "\r\n")?;
        return Ok(false);
      },
      Key::Enter => {
        write(out, "\r\n")?;
        return Ok(value);
      },
      Key::Esc => return Err(PromptEnd::Back),
      Key::CtrlC => return Err(PromptEnd::Cancelled),
      Key::Backspace | Key::Char(_) => {},
    }
  }
}

#[cfg(test)]
#[path = "tests/prompts.rs"]
mod tests;
