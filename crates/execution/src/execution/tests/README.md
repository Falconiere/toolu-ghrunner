# execution/tests/

**What belongs here:** flat test files for private execution-module behavior.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `step_timeout.rs` | Checks stalled action resolution against a parent deadline and cancellation. |
| `job_cancellation.rs` | Real cleanup processes share one cancellation deadline and are reaped on expiry. |
