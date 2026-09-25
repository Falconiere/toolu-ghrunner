# job_runner/

**What belongs here:** the prepared job's step execution setup, including
action prefetch and the acquired job's run defaults.

**What does NOT belong here:** workspace setup, service startup, or teardown;
those stay in `job_runner.rs`.

| File | Primary item | Purpose |
| --- | --- | --- |
| `prepared.rs` | `execute` | Starts prefetch, resolves live job defaults, runs the job body, and stops future fetches. |
