# job_lifecycle/

**What belongs here:** Run Service completion payload construction and delivery.

**What does NOT belong here:** broker polling, cancellation, or engine execution; those stay in `job_lifecycle.rs` and `execution_loop.rs`.

| File | Primary item | Purpose |
| --- | --- | --- |
| `completion.rs` | `report_completion` | Serializes filtered outputs with the job verdict and retries transient reports. |
