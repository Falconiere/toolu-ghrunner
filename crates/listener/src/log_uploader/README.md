# log_uploader/

**What belongs here:** step-level and job-level log upload to GitHub's
Results Service — the per-step streaming actor and the gzip-blob 3-phase
upload (signed URL → PUT → finalize metadata).

**What does NOT belong here:** the raw HTTP calls (signed-URL request, blob
PUT, metadata finalize) live in `wire::net::results_service` /
`wire::net::log_upload`, which this module calls into. Live WebSocket
streaming to the GH UI is `wire::reporting::live_log`, not this module.
Per-step result aggregation (status/conclusion, not logs) is
`listener::step_reporter::StepCollector`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `streamer.rs` | `StreamerConfig` / `spawn` | Spawns a per-step tokio actor that receives log lines over an mpsc channel and gzip-uploads the accumulated blob on finalize. |
| `upload.rs` | `upload_compressed_step_logs` / `upload_job_logs` | Shared 3-phase upload helpers (get signed URL, PUT gzipped blob with one retry, finalize metadata) for both step-level and combined job-level logs. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
