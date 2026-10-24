//! #208: a drain stops the idle poll loop, never an acquired job.
//!
//! Drives a REAL `poll_and_execute` against one wiremock `MockServer`
//! standing in for both the broker and the Run Service — the fixture of
//! `early_ack.rs` (a captured-shape job message with one real shell step).

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use shared::{ActionStep, Conclusion};

use crate::early_ack::{
  TestResult, job_message, make_config, make_ctx, mount_acknowledge, mount_acquire_job,
  mount_complete_job, mount_message_poll,
};
use crate::job_lifecycle::poll_and_execute;

/// How many requests `server` received for `wanted`.
async fn count(server: &MockServer, wanted: &str) -> TestResult<usize> {
  let requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;
  Ok(requests.iter().filter(|r| r.url.path() == wanted).count())
}

/// A broker whose `/message` long-poll never has work (202 after 200 ms).
async fn idle_broker() -> MockServer {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/message"))
    .respond_with(ResponseTemplate::new(202).set_delay(Duration::from_millis(200)))
    .mount(&server)
    .await;
  server
}

/// A broker + Run Service serving one job whose single step runs `script`.
async fn job_broker(script: &str) -> TestResult<MockServer> {
  let server = MockServer::start().await;
  let uri = server.uri();
  let mut msg = job_message(&uri, "drain-job");
  msg.steps = vec![ActionStep::script("step-1", script, "")];
  mount_message_poll(&server, "rr-drain", &uri).await;
  mount_acquire_job(&server, "plan-1", &msg).await?;
  mount_acknowledge(&server, 200).await;
  mount_complete_job(&server).await;
  Ok(server)
}

/// The conclusion the runner reported in its `completejob`.
async fn reported_conclusion(server: &MockServer) -> TestResult<String> {
  let requests = server
    .received_requests()
    .await
    .ok_or("request recording was disabled")?;
  let complete = requests
    .iter()
    .find(|r| r.url.path() == "/completejob")
    .ok_or("no completejob request")?;
  let body: serde_json::Value = serde_json::from_slice(&complete.body)?;
  let conclusion = body.get("conclusion").and_then(serde_json::Value::as_str);
  Ok(conclusion.ok_or("no conclusion")?.to_owned())
}

/// Idle when the drain fires: the loop ends with no job within a couple of
/// seconds and nothing is acquired.
#[tokio::test]
async fn a_drain_while_idle_ends_the_poll_loop_without_a_job() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let server = idle_broker().await;
  let mut ctx = make_ctx(server.uri(), make_config(&dir));
  let drain = CancellationToken::new();
  let trigger = drain.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(500)).await;
    trigger.cancel();
  });
  let started = Instant::now();
  let result =
    tokio::time::timeout(Duration::from_secs(10), poll_and_execute(&mut ctx, &drain)).await?;
  assert!(matches!(result, Ok(None)), "got {result:?}");
  assert!(
    started.elapsed() < Duration::from_secs(3),
    "the drain must end the idle loop promptly; took {:?}",
    started.elapsed()
  );
  assert!(count(&server, "/message").await? >= 1, "it was polling");
  assert_eq!(count(&server, "/acquirejob").await?, 0);
  assert!(!ctx.cancel.is_cancelled(), "a drain is not a cancellation");
  Ok(())
}

/// A drain that already fired takes no job, even one the broker is ready
/// to hand out.
#[tokio::test]
async fn a_drain_fired_before_polling_takes_no_job() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let server = job_broker("true").await?;
  let mut ctx = make_ctx(server.uri(), make_config(&dir));
  let drain = CancellationToken::new();
  drain.cancel();
  let result = poll_and_execute(&mut ctx, &drain).await;
  assert!(matches!(result, Ok(None)), "got {result:?}");
  assert_eq!(count(&server, "/acquirejob").await?, 0);
  Ok(())
}

/// A drain after the acquire never stops the job: it runs its real step to
/// the end and reports `succeeded`.
#[tokio::test]
async fn a_drain_after_the_acquire_lets_the_job_finish() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let server = job_broker("sleep 1").await?;
  let mut ctx = make_ctx(server.uri(), make_config(&dir));
  let drain = CancellationToken::new();
  // Fire the drain as soon as the acquire has been seen, while the
  // job's real `sleep 1` step is still running.
  let job = poll_and_execute(&mut ctx, &drain);
  tokio::pin!(job);
  let deadline = Instant::now() + Duration::from_secs(60);
  let result = loop {
    tokio::select! {
      result = &mut job => break result,
      () = tokio::time::sleep(Duration::from_millis(20)) => {
        if !drain.is_cancelled() && count(&server, "/acquirejob").await? > 0 {
          drain.cancel();
        }
        if Instant::now() > deadline {
          return Err("the job did not finish".into());
        }
      },
    }
  };
  assert!(drain.is_cancelled(), "the drain fired mid-job");
  assert!(
    matches!(result, Ok(Some(Conclusion::Success))),
    "got {result:?}"
  );
  assert_eq!(count(&server, "/acquirejob").await?, 1);
  assert_eq!(reported_conclusion(&server).await?, "succeeded");
  Ok(())
}
