# job_runner/

**What belongs here:** job entry sequencing, HTTP-client setup, prepared-step
execution, action prefetch, acquired run defaults, and the final "Complete job"
row (job outputs and environment URL).

**What does NOT belong here:** workspace/service implementations and teardown
helpers; `entry.rs` orchestrates those through the parent `job_runner.rs`.

| File | Primary item | Purpose |
| --- | --- | --- |
| `prepared.rs` | `execute` | Starts prefetch, resolves live job defaults, runs the job body, and stops future fetches. |
| `complete_step.rs` | `run_complete_step` | The "Complete job" row (upstream `FinalizeJob`): numbered after every post, evaluates job outputs then the environment URL, and reports its warnings/errors on that row (#88). |
| `outputs.rs` | `evaluate_final_outputs` | Resolves acquired job outputs inside "Complete job", warns `Skip output '<name>' since it may contain secret.`, and preserves cancellation. |
| `entry.rs` | `run` | Initializes a job, runs main/posts, and tears down resources with separate job cancellation and shutdown. |
| `job_http.rs` | `build_job_client` | The job's HTTP clients (job client, accelerated upstream) from `RunnerConfig.network`, the host-step network export + proxy-credential masks, and the job / service container proxy variables (#94). |
