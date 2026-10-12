//! `lifecycle_client`: masker registration, fail-closed config and the
//! once-per-change memo.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use shared::{NetworkConfig, RunnerError, SecretMasker};

use super::{NetworkMemo, lifecycle_client};

fn cfg(proxy: &str) -> NetworkConfig {
  NetworkConfig {
    proxy: Some(proxy.to_owned()),
    ..NetworkConfig::default()
  }
}

#[test]
fn memo_reports_only_changes() {
  let mut memo = NetworkMemo::default();
  let first = vec!["a".to_owned()];
  assert!(memo.secrets_changed(&first));
  assert!(!memo.secrets_changed(&first));
  assert!(memo.secrets_changed(&["b".to_owned()]));
  assert!(memo.line_changed("proxy none"));
  assert!(!memo.line_changed("proxy none"));
  assert!(memo.line_changed("proxy http://p:1 (env)"));
}

/// An in-memory `tracing` sink: every formatted event's bytes.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    if let Ok(mut sink) = self.0.lock() {
      sink.extend_from_slice(buf);
    }
    Ok(buf.len())
  }

  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

impl Captured {
  /// The `network: …` lines logged so far.
  fn network_lines(&self) -> Vec<String> {
    let text = self
      .0
      .lock()
      .map(|sink| String::from_utf8_lossy(&sink).into_owned())
      .unwrap_or_default();
    text
      .lines()
      .filter_map(|line| {
        line
          .split_once("network: ")
          .map(|(_, rest)| rest.to_owned())
      })
      .collect()
  }
}

#[test]
fn credentials_are_masked_and_repeat_passes_are_memoised() -> Result<(), RunnerError> {
  let captured = Captured::default();
  let sink = captured.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_ansi(false)
    .with_writer(move || sink.clone())
    .finish();
  let masker = Mutex::new(SecretMasker::new());
  let mut memo = NetworkMemo::default();
  let first = cfg("http://u:sekritpw@127.0.0.1:3128");
  let changed = cfg("http://u:sekritpw@127.0.0.1:3129");
  tracing::subscriber::with_default(subscriber, || {
    // Same settings twice (a backoff retry), then an edit.
    lifecycle_client(&first, &masker, Duration::from_secs(5), &mut memo)?;
    lifecycle_client(&first, &masker, Duration::from_secs(5), &mut memo)?;
    lifecycle_client(&changed, &masker, Duration::from_secs(5), &mut memo)?;
    Ok::<(), RunnerError>(())
  })?;
  assert_eq!(
    captured.network_lines(),
    vec![
      "proxy http://u:***@127.0.0.1:3128 (config), no_proxy 0 entries".to_owned(),
      "proxy http://u:***@127.0.0.1:3129 (config), no_proxy 0 entries".to_owned(),
    ]
  );
  let line_out = masker
    .lock()
    .map(|guard| guard.mask("password sekritpw here").into_owned())
    .unwrap_or_default();
  assert_eq!(line_out, "password *** here");
  Ok(())
}

#[test]
fn an_unusable_proxy_fails_closed_naming_the_setting() {
  let masker = Mutex::new(SecretMasker::new());
  let result = lifecycle_client(
    &cfg("ftp://u:sekritpw@x"),
    &masker,
    Duration::from_secs(5),
    &mut NetworkMemo::default(),
  );
  let msg = match result {
    Err(RunnerError::Config(msg)) => msg,
    Err(other) => format!("expected a Config error, got {other:?}"),
    Ok(_) => "expected an error, got a client".to_owned(),
  };
  assert_eq!(
    msg,
    "[network] proxy ftp://u:***@x: scheme must be http or https"
  );
}
