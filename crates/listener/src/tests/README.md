# listener/tests/

**What belongs here:** flat sibling tests for listener internals and the reporting path.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `completejob.rs` | Captured issue-88 jobs through `run_acquired_job` → `report_completion`: environment URL, secret suppression, billing owner, upstream `StepResult` keys and identity, infrastructure category latch, retry and cancellation. |
| `annotation_reporting.rs` | Captured-job shell/composite annotations through a recorded Run Service POST. |
| `early_ack.rs` | Broker acknowledgment ordering. |
| `execution_loop.rs` | Execution and renewal behavior. |
| `finalize_split.rs` | Completion and teardown ordering. |
| `helpers.rs` | Listener helper behavior. |
| `multiline_mask.rs` | Real job masking through listener sinks. |
| `post_results.rs` | Captured job replay through `StepCollector` and `CompleteJobRequest` serialization. |
| `startup_overlap.rs` | Startup reporting overlap. |
| `step_report_queue.rs` | Queued reporting against a broker endpoint. |
| `step_report_queue_unit.rs` | Report queue deadline and batching behavior. |
| `support.rs` | Recording endpoint and shared test support. |
| `watchdog_retry.rs` | Watchdog retry behavior. |
| `watchdog_trip.rs` | Watchdog cancellation behavior. |
| `runner_update_policy.rs` | Idle/busy refresh warning, safe opaque bodies, cancellation and cursor continuity. |
| `setup_step.rs` | Captured permissions, identity and secret-source diagnostics. |
| `setup_forwarding.rs` | Production replay, masked setup logs, early failure and cancellation. |
| `step_summary.rs` | Failed summary uploads still drain later queued documents. |
