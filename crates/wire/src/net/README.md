# net/

**What belongs here:** every async HTTP I/O call the runner makes to
GitHub's GitHub Actions JIT protocol — auth token exchange, device-flow
login, App-manifest onboarding, broker session lifecycle, long-poll
message acquire/acknowledge, run-service acquire/renew/complete, results
Twirp RPCs, log blob upload, GHES V1 service discovery — plus the toolu.sh
`/runner/*` job-claim calls a paired machine polls (#206).

**What does NOT belong here:** request/response type definitions and pure
URL/JWT/crypto builders live in `protocol` (sync, no I/O, no network) —
this module is the one-way async transport layer on top of them: every
`pub async fn` here takes a `reqwest::Client` plus a `protocol` request
type and returns a `protocol` response type or `shared::RunnerError`.
Higher-level domain wrappers and reporting-only types live in
`crate::reporting`, which composes these thin transport functions.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `app_manifest.rs` | `CallbackServer` / `convert_manifest_code` | Loopback HTTP server for the GitHub App manifest flow's redirect callback, plus the `app-manifests/{code}/conversions` POST. |
| `auth.rs` | `authenticate` / `exchange_token` | POSTs the signed JWT to swap it for an OAuth2 access token (the JWT itself is built in `protocol::auth`). A 4xx OAuth `invalid_client` reply (the registration was deleted on GitHub) is `RunnerError::Auth`. |
| `device_auth.rs` | `request_device_code` / `poll_for_token` | GitHub OAuth device-authorization flow: request a device/user code, then poll until an access token or terminal failure. |
| `github_app.rs` | `find_installation` / `create_installation_token` | GitHub App calls authenticated with the App JWT (`protocol::app_jwt`): the installation covering a repo or org, the App's installation list, and installation-token minting; `reuse_deadline` is the token-cache rule. 429/5xx are `Network`, every other non-2xx `Auth`. |
| `log_upload.rs` | `upload_block_blob` / `upload_log` | Raw Azure Blob Storage PUT requests (BlockBlob and AppendBlob); the mode decision lives in `crate::reporting::log_upload`. |
| `messages.rs` | `poll_message` / `acknowledge_message` | HTTP transport for the broker long-poll loop; response decryption lives in `protocol::messages`. |
| `register.rs` | `register_jit` / `unregister_runner` | POSTs `generate-jitconfig` to mint a JIT registration, and DELETEs a runner registration — repo- or org-scoped (`RunnerScope`, `resolve_target`). |
| `runner_groups.rs` | `list_runner_groups` / `pick_group` | Lists an organization's runner groups and resolves a `--runner-group` name to its id. |
| `results_service.rs` | `update_workflow_steps` / signed-blob-URL + metadata calls | Twirp JSON-over-HTTP POSTs to the GitHub Actions Results Service. |
| `run_service.rs` | `acquire_job` / `renew_job` / `complete_job` | HTTP transport for the Actions Run Service job lifecycle; request/response shapes live in `crate::reporting::run_service`. |
| `session.rs` | `create_session` / `delete_session` | HTTP transport for creating and deleting the broker session; a 409 is `RunnerError::SessionConflict`. |
| `toolu_jobs.rs` | `TooluClient` / `TooluError` | toolu.sh `/runner/*` calls of a paired machine (#206): list queued jobs, claim, dispatched, heartbeat, complete, plus #207's `GET`/`PATCH`/`DELETE /runner/host` (`host`, `update_host`, `revoke_host`), with the host token as bearer. 401 is `Revoked`, 409 `Conflict`, 404 `NotFound`, 400 `Refused`, 429/5xx/transport `Transient`; bodies in `protocol::toolu_jobs` (jobs) and `protocol::toolu_pair` (`/runner/host`). |
| `toolu_pair.rs` | `pair_start` / `pair_token` / `enroll` | toolu.sh public pairing routes (#207): start a device-code pairing, poll it (a 400 `error` is a `PairPoll` outcome), redeem an enrollment token (401 `Revoked`). The wait loop is the caller's; bodies in `protocol::toolu_pair`. |
| `step_summary.rs` | `upload_step_summary` | Safe bounded summary signed-URL, blob and metadata requests. |
| `v1.rs` | `fetch_connection_data` / `fetch_timeline` / `post_timeline_record` | HTTP fetches for GHES V1 service discovery and timeline reporting; pure URL resolvers live in `protocol::v1`. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
