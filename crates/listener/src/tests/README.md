# listener/tests/

**What belongs here:** flat sibling tests for listener internals and the reporting path.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `capture.rs` | Real acquire body through `capture::write_acquired`: exact JSON, 0600/0700 modes, contained file name, unwritable data dir. |
| `completejob.rs` | Captured issue-88 jobs through `run_acquired_job` → `report_completion`: environment URL, secret suppression, billing owner, upstream `StepResult` keys and identity, infrastructure category latch, retry and cancellation. |
| `annotation_reporting.rs` | Captured-job shell/composite annotations through a recorded Run Service POST. |
| `broker_control.rs` | Committed V2 envelope shapes through the production message classifier: future types skipped with the original cursor, forced refresh vs known update messages, hosted shutdown, redelivered control messages. |
| `early_ack.rs` | Broker acknowledgment ordering. |
| `execution_loop.rs` | Execution and renewal behavior. |
| `finalize_split.rs` | Completion and teardown ordering. |
| `helpers.rs` | Listener helper behavior. |
| `job_outputs.rs` | The event forwarder keeps job outputs from a real acquired engine run for completion. |
| `multiline_mask.rs` | Real job masking through listener sinks. |
| `post_results.rs` | Captured job replay through `StepCollector` and `CompleteJobRequest` serialization. |
| `startup_overlap.rs` | Startup reporting overlap. |
| `step_report_queue.rs` | Queued reporting against a broker endpoint. |
| `step_report_queue_unit.rs` | Report queue deadline and batching behavior. |
| `step_context_identity.rs` | Report serialization of a skipped step with the wire ID and conclusion from the sanitized acquired job. |
| `support.rs` | Recording endpoint and shared test support. |
| `watchdog_retry.rs` | Watchdog retry behavior. |
| `watchdog_trip.rs` | Watchdog cancellation behavior. |
| `runner_update_policy.rs` | Idle/busy refresh warning, safe opaque bodies, cancellation and cursor continuity. |
| `session_conflict.rs` | Live-captured 409 replayed through `create_session`: bounded retry, recovery, exact attempt count, unrelated failures and cancellation. |
| `setup_step.rs` | Captured permissions, identity and secret-source diagnostics. |
| `setup_forwarding.rs` | Production replay, masked setup logs, early failure and cancellation. |
| `step_summary.rs` | Failed summary uploads still drain later queued documents. |
