//! Real-data parse test for the committed `tests/fixtures/job_message.json`
//! fixture — the shared `[H]` input that drives all hermetic gh-compat tests.

use shared::AgentJobRequestMessage;

/// The committed, self-contained job-message fixture (real github.com V2 shape).
const FIXTURE: &str = include_str!("fixtures/job_message.json");

const EXPECTED_SERVER_URL: &str =
  "https://pipelines.actions.githubusercontent.com/aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789/";

#[test]
fn fixture_parses_into_agent_job_request_message() {
  let msg: AgentJobRequestMessage =
    serde_json::from_str(FIXTURE).expect("fixture must deserialize into AgentJobRequestMessage");

  assert!(
    !msg.message_type.is_empty(),
    "messageType must be non-empty"
  );
  assert!(!msg.plan.plan_id.is_empty(), "planId must be present");
  assert!(!msg.job_id.is_empty(), "jobId must be present");
  assert!(
    msg.steps.len() >= 2,
    "expected >=2 steps, got {}",
    msg.steps.len()
  );

  let token = msg
    .variables
    .get("system.github.token")
    .expect("system.github.token variable must be present");
  assert!(
    token.is_secret,
    "system.github.token must have isSecret == true"
  );

  let non_secret = msg
    .variables
    .get("system.runnerGroupName")
    .expect("system.runnerGroupName variable must be present");
  assert!(
    !non_secret.is_secret,
    "system.runnerGroupName must be isSecret == false"
  );

  let server_url = msg
    .server_url()
    .expect("server_url() must resolve from SystemVssConnection endpoint");
  assert_eq!(
    server_url, EXPECTED_SERVER_URL,
    "server_url must equal endpoint url"
  );

  let feed = msg
    .feed_stream_url()
    .expect("feed_stream_url() must resolve from SystemVssConnection data");
  assert!(
    feed.starts_with("wss://"),
    "feed_stream_url must convert https->wss: {feed}"
  );
  assert!(
    !feed.contains("https://"),
    "feed_stream_url must drop https scheme: {feed}"
  );

  assert!(
    msg.run_service_url().is_some(),
    "runServiceUrl must be present for V2"
  );
}
