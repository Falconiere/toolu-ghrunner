//! `wizard::verify` decision (AC-9) over runner log tails in the real
//! tracing JSON format (the online fixture's marker line mirrors the one
//! `listener::handler` emits): a confirmed-online line only counts when the
//! supervisor is also active, and every not-online combination reports a
//! distinct reason.

use std::error::Error;

use observability::wizard::verify::{VerifyOutcome, verify_decision};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The online marker the runner emits once it starts long-polling.
const MARKER: &str = "long-polling for jobs";

const ONLINE_LOG: &str = include_str!("fixtures/wizard/runner-online.log");
const OFFLINE_LOG: &str = include_str!("fixtures/wizard/runner-offline.log");

#[test]
fn active_service_with_marker_is_online() {
  assert!(
    ONLINE_LOG.contains(MARKER),
    "fixture must carry the online marker"
  );
  assert_eq!(
    verify_decision(true, ONLINE_LOG, MARKER),
    VerifyOutcome::Online
  );
}

#[test]
fn inactive_service_is_unconfirmed_even_with_marker() -> TestResult {
  let VerifyOutcome::Unconfirmed(reason) = verify_decision(false, ONLINE_LOG, MARKER) else {
    return Err("inactive service must not be Online".into());
  };
  assert_eq!(reason, "service not active");
  Ok(())
}

#[test]
fn active_service_without_marker_is_unconfirmed() -> TestResult {
  assert!(
    !OFFLINE_LOG.contains(MARKER),
    "offline fixture must lack the marker"
  );
  let VerifyOutcome::Unconfirmed(reason) = verify_decision(true, OFFLINE_LOG, MARKER) else {
    return Err("a missing marker must not read as Online".into());
  };
  assert_eq!(reason, "no online marker in log yet");
  Ok(())
}
