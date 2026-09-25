//! Tests for Docker's typed health-check create request contract.

use bollard::models::ContainerCreateBody;

use super::apply_health_options;

/// Docker creates shell-form health commands as `CMD-SHELL`, with duration
/// options expressed as checked nanoseconds in the Engine API request.
#[test]
fn maps_health_options_to_docker_healthcheck_contract() {
  let options = vec![
    "--health-cmd".to_owned(),
    "curl --fail http://localhost:8080/ready".to_owned(),
    "--health-interval".to_owned(),
    "1m30s".to_owned(),
    "--health-timeout".to_owned(),
    "0.75s".to_owned(),
    "--health-retries".to_owned(),
    "3".to_owned(),
    "--health-start-period".to_owned(),
    "1m".to_owned(),
    "--health-start-interval".to_owned(),
    "500ms".to_owned(),
  ];
  let mut body = ContainerCreateBody::default();

  let result = apply_health_options(&options, &mut body);

  assert!(result.is_ok(), "Docker health options must map: {result:?}");
  assert_eq!(
    body
      .healthcheck
      .as_ref()
      .and_then(|health| health.test.clone()),
    Some(vec![
      "CMD-SHELL".to_owned(),
      "curl --fail http://localhost:8080/ready".to_owned(),
    ])
  );
  assert_eq!(
    body.healthcheck.as_ref().and_then(|health| health.interval),
    Some(90_000_000_000)
  );
  assert_eq!(
    body.healthcheck.as_ref().and_then(|health| health.timeout),
    Some(750_000_000)
  );
  assert_eq!(
    body.healthcheck.as_ref().and_then(|health| health.retries),
    Some(3)
  );
  assert_eq!(
    body
      .healthcheck
      .as_ref()
      .and_then(|health| health.start_period),
    Some(60_000_000_000)
  );
  assert_eq!(
    body
      .healthcheck
      .as_ref()
      .and_then(|health| health.start_interval),
    Some(500_000_000)
  );
}

/// `--no-healthcheck` uses Docker's explicit `NONE` test sentinel.
#[test]
fn disables_healthcheck_with_docker_none_sentinel() {
  let mut body = ContainerCreateBody::default();
  let result = apply_health_options(&["--no-healthcheck".to_owned()], &mut body);

  assert!(result.is_ok(), "healthcheck disable must map: {result:?}");
  assert_eq!(
    body.healthcheck.and_then(|health| health.test),
    Some(vec!["NONE".to_owned()])
  );
}

/// Invalid durations, retry counts, and disabled-plus-configured healthchecks
/// are rejected before a Docker create request can be emitted.
#[test]
fn rejects_invalid_or_conflicting_health_options() {
  for options in [
    vec!["--health-interval".to_owned(), "1w".to_owned()],
    vec![
      "--health-timeout".to_owned(),
      "999999999999999999999h".to_owned(),
    ],
    vec!["--health-start-period".to_owned(), "-1s".to_owned()],
    vec!["--health-start-period".to_owned(), "1.2.3s".to_owned()],
    vec!["--health-start-period".to_owned(), "1ns".to_owned()],
    vec!["--health-retries".to_owned(), "-1".to_owned()],
    vec![
      "--no-healthcheck".to_owned(),
      "--health-cmd".to_owned(),
      "true".to_owned(),
    ],
  ] {
    let mut body = ContainerCreateBody::default();
    let result = apply_health_options(&options, &mut body);
    assert!(result.is_err(), "options must fail: {options:?}");
    assert!(body.healthcheck.is_none(), "invalid options must not map");
  }
}

/// Docker CLI's duration parser accepts the zero inheritance value and an
/// optional positive sign, while preserving checked nanosecond conversion.
#[test]
fn accepts_zero_and_explicit_positive_durations() {
  let options = vec![
    "--health-interval".to_owned(),
    "0".to_owned(),
    "--health-timeout".to_owned(),
    "+0.5s".to_owned(),
  ];
  let mut body = ContainerCreateBody::default();

  let result = apply_health_options(&options, &mut body);

  assert!(
    result.is_ok(),
    "Docker duration syntax must map: {result:?}"
  );
  assert_eq!(
    body.healthcheck.as_ref().and_then(|health| health.interval),
    Some(0)
  );
  assert_eq!(
    body.healthcheck.as_ref().and_then(|health| health.timeout),
    Some(500_000_000)
  );
}
