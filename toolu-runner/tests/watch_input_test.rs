//! Watch input mapping + cancel path: key → `Action` bindings, the
//! confirm modal, and AC-9 — `send_cancel` resolves the PID from a REAL
//! `.lock` body and delivers SIGINT to a live child process.

use std::error::Error;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use toolu_runner::watch::input::{Action, action_for};
use toolu_runner::watch::state::App;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn key(code: KeyCode) -> KeyEvent {
  KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn bindings_map_to_actions() {
  let app = App::new("t".to_owned());
  let cases = [
    (key(KeyCode::Char('q')), Action::Quit),
    (key(KeyCode::Esc), Action::Quit),
    (key(KeyCode::Char('c')), Action::RequestCancel),
    (key(KeyCode::Char('f')), Action::ToggleFollow),
    (key(KeyCode::Tab), Action::TogglePane),
    (key(KeyCode::Up), Action::MoveUp),
    (key(KeyCode::Char('k')), Action::MoveUp),
    (key(KeyCode::Down), Action::MoveDown),
    (key(KeyCode::Char('j')), Action::MoveDown),
    (key(KeyCode::Enter), Action::OpenSelected),
    (key(KeyCode::PageUp), Action::PageUp),
    (key(KeyCode::PageDown), Action::PageDown),
    (key(KeyCode::Char('z')), Action::None),
  ];
  for (k, want) in cases {
    assert_eq!(action_for(&app, k), want, "binding for {k:?}");
  }
  let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
  assert_eq!(action_for(&app, ctrl_c), Action::Quit);
}

#[test]
fn confirm_modal_swallows_keys_until_answered() {
  let mut app = App::new("t".to_owned());
  app.confirm_cancel = true;
  assert_eq!(
    action_for(&app, key(KeyCode::Char('y'))),
    Action::ConfirmCancel
  );
  assert_eq!(
    action_for(&app, key(KeyCode::Char('Y'))),
    Action::ConfirmCancel
  );
  // Every other key — including the quit binding — dismisses instead.
  assert_eq!(
    action_for(&app, key(KeyCode::Char('q'))),
    Action::DismissCancel
  );
  assert_eq!(
    action_for(&app, key(KeyCode::Char('n'))),
    Action::DismissCancel
  );
  assert_eq!(action_for(&app, key(KeyCode::Enter)), Action::DismissCancel);
}

#[test]
fn send_cancel_without_lock_reports_error() {
  let dir = tempfile::tempdir().expect("tempdir");
  let err = toolu_runner::watch::send_cancel(&dir.path().join(".lock"))
    .expect_err("missing lock must not succeed");
  assert!(err.contains("no lock file"), "unexpected error: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn send_cancel_delivers_sigint_to_lock_holder() -> TestResult {
  // AC-9: a live child traps INT and exits 42; the `.lock` body carries
  // its real PID, exactly as `toolu-runner run` writes it.
  let mut child = std::process::Command::new("sh")
    .arg("-c")
    .arg("trap 'exit 42' INT; echo ready; while :; do sleep 0.05; done")
    .stdout(std::process::Stdio::piped())
    .spawn()?;
  // Wait for the trap to be installed before signalling, else the raw
  // SIGINT kills the shell and the exit code never appears.
  {
    use std::io::{BufRead, BufReader};
    let stdout = child.stdout.take().ok_or("child stdout missing")?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    assert_eq!(line.trim(), "ready");
  }

  let dir = tempfile::tempdir()?;
  let lock_path = dir.path().join(".lock");
  let body = serde_json::json!({
    "pid": child.id(),
    "started_at": "2026-07-08T00:00:00Z",
    "config_path": "/tmp/config.toml",
  });
  std::fs::write(&lock_path, serde_json::to_vec(&body)?)?;

  let pid = toolu_runner::watch::send_cancel(&lock_path).map_err(|e| format!("cancel: {e}"))?;
  assert_eq!(pid, child.id());

  let deadline = Instant::now() + Duration::from_secs(5);
  let status = loop {
    if let Some(status) = child.try_wait()? {
      break status;
    }
    if Instant::now() > deadline {
      child.kill()?;
      return Err("child never reacted to SIGINT".into());
    }
    std::thread::sleep(Duration::from_millis(50));
  };
  assert_eq!(status.code(), Some(42), "child must exit via its INT trap");
  Ok(())
}
