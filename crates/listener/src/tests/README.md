# listener/tests/

**What belongs here:** flat sibling tests for listener internals and the reporting path.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `compat_103_docker_message.json` | Symlink to the tracked execution capture used to verify Docker Pull reporting through the listener. |
| `capture.rs` | Real acquire body through `capture::write_acquired`: exact JSON, 0600/0700 modes, contained file name, unwritable data dir. |
| `completejob.rs` | Captured issue-88 jobs through `run_acquired_job` → `report_completion`: environment URL, secret suppression, billing owner, upstream `StepResult` keys and identity, infrastructure category latch, retry and cancellation. |
| `annotation_reporting.rs` | Captured-job shell/composite annotations through a recorded Run Service POST. |
| `broker_control.rs` | Committed V2 envelope shapes through the production message classifier: future types skipped with the original cursor, forced refresh vs known update messages, hosted shutdown, redelivered control messages. |
| `drain.rs` | #208 drain: an idle poll loop ends with no job, a pre-fired drain takes none, a drain after the acquire lets the real job finish and report `succeeded`. |
| `early_ack.rs` | Broker acknowledgment ordering (its job fixture is shared with `drain.rs`). |
| `execution_loop.rs` | Forwarder masking into the combined disk spool, step-upload channel and live feed. |
| `execution_large_logs.rs` | Real child emits over 200 MiB, including a 10 MiB line: bounded RSS growth, exact masked gzip uploads, private/stale spool handling and cancellation cleanup. |
| `execution_large_logs_failure.rs` | Combined-spool creation failure leaves per-step uploads working through the production forwarder. |
| `finalize_split.rs` | Completion and teardown ordering. |
| `helpers.rs` | Listener helper behavior. |
| `job_outputs.rs` | The event forwarder keeps job outputs from a real acquired engine run for completion. |
| `multiline_mask.rs` | Real job masking through listener sinks. |
| `post_results.rs` | Captured job replay through `StepCollector` and `CompleteJobRequest` serialization. |
| `startup_overlap.rs` | Startup reporting overlap. |
| `step_report_queue.rs` | Queued reporting against a broker endpoint. |
| `step_report_queue_unit.rs` | Report queue deadline, batching and Docker setup terminal conclusions. |
| `step_context_identity.rs` | Report serialization of a skipped step with the wire ID and conclusion from the sanitized acquired job. |
| `support.rs` | Recording endpoint and shared test support. |
| `watchdog_missing_job.rs` | Immediate missing-job cancellation reports job-lock loss without claiming a timed outage. |
| `watchdog_override.rs` | Pure conclusion fold: untripped conclusions stay unchanged; tripped success stays successful; tripped non-success becomes failure with the exact annotation. |
| `watchdog_retry.rs` | Watchdog retry behavior. |
| `watchdog_trip.rs` | Persistent 401, malformed success and transient errors trip the watchdog; successful renewal resets its window and completion survives reconnect. |
| `runner_update_policy.rs` | Idle/busy refresh warning, safe opaque bodies, cancellation and cursor continuity. |
| `session_conflict.rs` | Live-captured 409 replayed through `create_session`: bounded retry, recovery, exact attempt count, unrelated failures and cancellation. |
| `setup_step.rs` | Captured permissions, identity and secret-source diagnostics. |
| `setup_forwarding.rs` | Production replay, masked setup logs, early failure and cancellation; capture-derived Docker Pull row forwarding and order. |
| `step_summary.rs` | Failed summary uploads still drain later queued documents. |
