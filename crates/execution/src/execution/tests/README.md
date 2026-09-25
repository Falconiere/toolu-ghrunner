# execution/tests/

**What belongs here:** flat test files for private execution-module behavior.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `job_environment.rs` | Issue 69 captured-envelope replay for environment layers, step precedence, file commands, and job isolation. |
| `job_environment_actions.rs` | Issue 69 real Node stages and repeated nested composite environment probes. |
| `step_timeout.rs` | Checks stalled action resolution against a parent deadline and cancellation. |
| `job_cancellation.rs` | Real cleanup processes share one cancellation deadline and are reaped on expiry. |
