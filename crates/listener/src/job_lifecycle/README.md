# job_lifecycle/

**What belongs here:** the idle broker poll loop (with its #208 drain) and Run Service completion payload construction and delivery.

**What does NOT belong here:** acquiring a job, the mid-job cancellation watcher, or engine execution; those stay in `job_lifecycle.rs` and `execution_loop.rs`.

| File | Primary item | Purpose |
| --- | --- | --- |
| `completion.rs` | `report_completion` | Serializes filtered outputs, the evaluated `environmentUrl`, the echoed `billingOwnerId` and any `infrastructureFailureCategory` with the job verdict, and retries transient reports with the identical body. |
| `idle.rs` | `poll_until_job` | Long-polls the broker until a job message, a shutdown/cancellation, or a drain while idle; a drain never reaches an acquired job. |
