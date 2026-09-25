# execution/tests/

**What belongs here:** flat test files for private execution-module behavior.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `post_drain.rs` | Elapsed-time check for one cancellation deadline shared by cleanup posts. |
| `step_timeout.rs` | Checks stalled action resolution against a parent deadline and cancellation. |
