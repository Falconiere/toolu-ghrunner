//! Watch input mapping + cancel path: key → `Action` bindings, the
//! confirm modal, and AC-9 — `send_cancel` resolves the PID from a REAL
//! `.lock` body and delivers SIGINT to a live child process.

use std::error::Error;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use observability::watch::input::{Action, action_for};
use observability::watch::state::App;

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
  let err = observability::watch::send_cancel(&dir.path().join(".lock"))
    .expect_err("missing lock must not succeed");
  assert!(err.contains("no lock file"), "unexpected error: {err}");
}

/// Spawn a `/bin/sh` child that traps SIGINT (exit 42) and block until it's
/// ready. A plain child is NOT named toolu-runner — used to test both real
/// signal delivery and the identity gate's refusal.
#[cfg(unix)]
fn spawn_trap_child() -> TestResult<std::process::Child> {
  use std::io::{BufRead, BufReader};
  let mut child = std::process::Command::new("/bin/sh")
    .arg("-c")
    .arg("trap 'exit 42' INT; echo ready; while :; do sleep 0.05; done")
    .stdout(std::process::Stdio::piped())
    .spawn()?;
  // Wait for the trap to be installed before signalling, else the raw SIGINT
  // kills the process and the exit code never appears.
  let stdout = child.stdout.take().ok_or("child stdout missing")?;
  let mut line = String::new();
  BufReader::new(stdout).read_line(&mut line)?;
  assert_eq!(line.trim(), "ready");
  Ok(child)
}

/// Wait up to 5s for `child` to exit; returns its status.
#[cfg(unix)]
fn wait_for_exit(child: &mut std::process::Child) -> TestResult<std::process::ExitStatus> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    if let Some(status) = child.try_wait()? {
      return Ok(status);
    }
    if Instant::now() > deadline {
      return Err("child never exited".into());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
}

#[cfg(unix)]
#[tokio::test]
async fn deliver_sigint_stops_a_running_child() -> TestResult {
  // AC-9 (delivery): SIGINT reaches the target PID; the child exits via its
  // INT trap (code 42).
  let mut child = spawn_trap_child()?;
  let outcome = (|| {
    observability::watch::deliver_sigint(child.id()).map_err(|e| format!("deliver: {e}"))?;
    let status = wait_for_exit(&mut child)?;
    assert_eq!(status.code(), Some(42), "child must exit via its INT trap");
    Ok(())
  })();
  let _ = child.kill();
  let _ = child.wait();
  outcome
}

#[cfg(unix)]
#[tokio::test]
async fn send_cancel_refuses_non_runner_pid() -> TestResult {
  // AC-9 (identity gate): a lock pointing at a live NON-toolu-runner PID
  // (a plain sh child) is refused — no signal is sent.
  let mut child = spawn_trap_child()?;
  let dir = tempfile::tempdir()?;
  let lock_path = dir.path().join(".lock");
  let body = serde_json::json!({
    "pid": child.id(),
    "started_at": "2026-07-08T00:00:00Z",
    "config_path": "/tmp/config.toml",
  });
  std::fs::write(&lock_path, serde_json::to_vec(&body)?)?;

  let outcome: TestResult = match observability::watch::send_cancel(&lock_path) {
    Ok(_) => Err("send_cancel must refuse a non-runner pid".into()),
    Err(e) if e.contains("not a toolu-runner process") => Ok(()),
    Err(e) => Err(format!("unexpected error: {e}").into()),
  };
  // The child must still be alive (no signal was sent).
  let still_running = child.try_wait()?.is_none();
  let _ = child.kill();
  let _ = child.wait();
  assert!(
    still_running,
    "refused cancel must not have signalled the child"
  );
  outcome
}
