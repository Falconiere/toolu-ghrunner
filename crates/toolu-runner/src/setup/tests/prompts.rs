//! Prompt logic over real byte streams: arrows, space, `a`, esc and
//! ctrl+c bytes in — selections, toggles and cancels out. Secrets never
//! reach the writer.

use super::*;

fn keys(bytes: &[u8]) -> ByteKeys {
  ByteKeys::new(bytes)
}

#[test]
fn select_moves_with_arrows_and_confirms() {
  let options = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
  let mut out = Vec::new();
  let chosen = run_select(&mut keys(b"\x1b[B\x1b[B\r"), &mut out, "Pick", &options, 0).unwrap();
  assert_eq!(chosen, 2);
}

#[test]
fn select_ctrl_c_cancels_and_esc_goes_back() {
  let options = vec!["one".to_owned()];
  let mut out = Vec::new();
  assert!(matches!(
    run_select(&mut keys(b"\x03"), &mut out, "Pick", &options, 0),
    Err(PromptEnd::Cancelled)
  ));
  let mut out = Vec::new();
  assert!(matches!(
    run_select(&mut keys(b"\x1b"), &mut out, "Pick", &options, 0),
    Err(PromptEnd::Back)
  ));
}

#[test]
fn multi_select_toggles_and_all() {
  let options = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
  let mut out = Vec::new();
  // space on first, down, space on second, enter
  let chosen = run_multi_select(
    &mut keys(b" \x1b[B \r"),
    &mut out,
    "Repos",
    &options,
    &[false, false, false],
  )
  .unwrap();
  assert_eq!(chosen, vec![0, 1]);
  let mut out = Vec::new();
  let chosen =
    run_multi_select(&mut keys(b"a\r"), &mut out, "Repos", &options, &[false; 3]).unwrap();
  assert_eq!(chosen, vec![0, 1, 2]);
}

#[test]
fn secret_never_echoes_typed_bytes() {
  let mut out = Vec::new();
  let value = run_secret(&mut keys(b"ghp_secretvalue\r"), &mut out, "Token").unwrap();
  assert_eq!(value, "ghp_secretvalue");
  let rendered = String::from_utf8(out).unwrap();
  assert!(
    !rendered.contains("ghp_secretvalue"),
    "secret echoed: {rendered}"
  );
  assert!(rendered.contains("••••"));
}

#[test]
fn input_defaults_and_edits() {
  let mut out = Vec::new();
  let value = run_input(&mut keys(b"\r"), &mut out, "Name", "host-1").unwrap();
  assert_eq!(value, "host-1");
  let mut out = Vec::new();
  let value = run_input(&mut keys(b"abc\x7f\r"), &mut out, "Name", "host-1").unwrap();
  assert_eq!(value, "ab");
}

#[test]
fn parse_key_reads_escape_sequences() {
  assert_eq!(parse_key(b"\x1b[A"), Some((Key::Up, 3)));
  assert_eq!(parse_key(b"\x1b[B"), Some((Key::Down, 3)));
  assert_eq!(parse_key(b"\x03"), Some((Key::CtrlC, 1)));
  assert_eq!(parse_key(b"\x1b"), Some((Key::Esc, 1)));
}

#[test]
fn input_accepts_spaces_and_utf8() {
  let mut out = Vec::new();
  let value = run_input(&mut keys("my runner\r".as_bytes()), &mut out, "Name", "x").unwrap();
  assert_eq!(value, "my runner");
  let mut out = Vec::new();
  let value = run_input(&mut keys("caf\u{e9}\r".as_bytes()), &mut out, "Name", "x").unwrap();
  assert_eq!(value, "caf\u{e9}");
  let mut out = Vec::new();
  let value = run_secret(&mut keys(b"a b\r"), &mut out, "Token").unwrap();
  assert_eq!(value, "a b");
}
