//! `lifecycle_client`: masker registration, fail-closed config and the
//! once-per-change memo.

use std::sync::Mutex;
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

#[test]
fn credentials_are_masked_and_repeat_passes_are_memoised() -> Result<(), RunnerError> {
  let masker = Mutex::new(SecretMasker::new());
  let mut memo = NetworkMemo::default();
  let network = cfg("http://u:sekritpw@127.0.0.1:3128");
  lifecycle_client(&network, &masker, Duration::from_secs(5), &mut memo)?;
  let described = network::describe(&network)?;
  assert!(
    !memo.line_changed(&described),
    "first pass recorded the line"
  );
  assert!(
    !memo.secrets_changed(&network::proxy_secrets(&network)),
    "first pass recorded the secrets"
  );
  // A second pass with the same settings still builds a client.
  lifecycle_client(&network, &masker, Duration::from_secs(5), &mut memo)?;
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
