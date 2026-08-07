# reporting/

**What belongs here:** domain types and thin async wrappers for the
Actions Run Service and Results Service — request/response shapes,
protocol-version detection, live-log WebSocket streaming, and the
BlockBlob-vs-AppendBlob upload-mode decision.

**What does NOT belong here:** the actual HTTP calls (the
`reqwest::Client` request/response round trip) live in `crate::net`,
which the wrappers here delegate to — this module owns shapes and
decisions, not sockets. Building the JWT/RSA auth handshake or parsing
the JIT config envelope is `protocol`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `feature_detection.rs` | `ProtocolVersion` / `detect_protocol_version` | Detects V1 (GHES timeline API) vs. V2 (github.com Twirp Results Service) from the job message. |
| `live_log.rs` | `LiveLogStreamer` | Streams log lines to the GitHub Actions UI over a WebSocket to the job's `FeedStreamUrl`, batching and flushing on a timer/threshold; falls back silently on connect failure. |
| `log_upload.rs` | `LogUploader` / `UploadMode` | Picks BlockBlob vs. AppendBlob by content size and calls the matching `crate::net` upload function; also formats timestamped log lines. |
| `results_service.rs` | `update_workflow_steps` / signed-blob-URL + metadata wrappers | Thin async wrappers over `crate::net::results_service`, re-exporting the Twirp request/response types from `results_types`. |
| `results_types.rs` | `WorkflowStepsUpdateRequest` / `StepUpdateEntry` | Twirp request/response types for the Results Service, snake_case JSON matching the C# runner's wire format. |
| `run_service.rs` | `AcquireJobRequest` / `acquire_job` / `renew_job` / `complete_job` | Request/response shapes plus thin async wrappers for the Run Service's acquire/renew/complete job lifecycle. |
| `types.rs` | `Status` / `Conclusion` / `StepResult` / `Annotation` | Shared Twirp status/conclusion enums and the per-step result / annotation shapes used across `run_service` and `results_types`. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
