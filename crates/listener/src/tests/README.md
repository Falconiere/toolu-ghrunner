# listener/tests/

**What belongs here:** flat sibling tests for listener internals and the reporting path.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
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
