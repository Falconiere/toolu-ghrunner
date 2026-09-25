//! Real processes share one cancellation budget across consecutive cleanup stages.

use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::JobCancellation;
use crate::execution::cgroup_join::spawn_in_cgroup;
use crate::execution::context::ExecutionContext;
use crate::execution::step_timeout::{WaitOutcome, wait_bounded};
use shared::RunnerError;

#[tokio::test]
async fn cleanup_processes_share_one_deadline() -> Result<(), Box<dyn std::error::Error>> {
  let request = CancellationToken::new();
  request.cancel();
  let signal = JobCancellation::with_grace(
    request,
    CancellationToken::new(),
    Duration::from_millis(500),
  );
  let deadline = signal.deadline().ok_or("deadline missing")?;
  let ctx = ExecutionContext::new_for_test();
  let first = signal.watch_step(ctx.eval_context(), "always()");
  let mut child = spawn_in_cgroup(Command::new("sleep").arg("0.05"), None).await?;
  assert!(
    matches!(wait_bounded(&mut child, None, &first.cancel, RunnerError::ScriptHandler).await?, WaitOutcome::Exited(status) if status.success())
  );
  let second = signal.watch_step(ctx.eval_context(), "always()");
  let mut child = spawn_in_cgroup(Command::new("sleep").arg("30"), None).await?;
  let outcome = tokio::time::timeout(
    Duration::from_secs(3),
    wait_bounded(&mut child, None, &second.cancel, RunnerError::ScriptHandler),
  )
  .await??;
  assert!(matches!(outcome, WaitOutcome::Cancelled));
  assert_eq!(signal.deadline(), Some(deadline));
  assert!(signal.force.is_cancelled());
  assert!(child.try_wait()?.is_some());
  Ok(())
}
