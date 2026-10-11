# CompleteJob / StepResult payload parity (#88) — Design

**Date:** 2026-10-10   **Status:** Approved (rev 5)   **Author:** Claude (epic #67 worker)   **Topic:** `environmentUrl`, `billingOwnerId`, `infrastructureFailureCategory` and upstream-shaped `StepResult`s on `completejob`

Brainstorm: `docs/toolu/brainstorms/2026-10-10-completejob-stepresult-parity.md`.
Upstream pin: actions/runner `cab9d1c3901e45c7705889c4f88284fdd93f4ae5` (2.337.0).

## Problem

1. A job with `environment: { name, url }` never sends `environmentUrl`, so
   GitHub's deployment has no "View deployment" link. toolu does not
   deserialize the job message's `actionsEnvironment`.
2. `CompleteJobRequest` lacks `billingOwnerId` and
   `infrastructureFailureCategory`. Upstream sets the category from the first
   categorized infrastructure issue: action preparation failures
   `resolve_action`, `error_download_action`, `invalid_action_download`
   (`ActionManager.cs` L119-140) and the debugger's `debugger_tunnel_failure`.
3. toolu's `StepResult` is not the official wire shape. Upstream
   (`StepResult.cs`, serialized by `VssJsonMediaTypeFormatter`) sends
   snake_case keys (`external_id`, `action_name`, `ref`, `type`,
   `started_at`, `completed_at`, `completed_log_url`, `completed_log_lines`)
   and camelCase string enums (`"status":"completed"`,
   `"conclusion":"succeeded"`). toolu sends camelCase keys, Twirp numbers and
   an extra `outcome`, and no action name/ref/type.
4. Upstream evaluates job outputs and the environment URL inside a
   "Complete job" step; toolu has no such row (deferred here by #99). The #70
   "skip secret output" warning is keyed by the job id, never belongs to a
   completed step, and is dropped before `completejob`.

## Non-Goals

1. No step debugger, so `debugger_tunnel_failure` is never produced.
2. No GHES V1 `JobEvent` completion: toolu never posts `JobEvent`
   (`protocol::v1::JobEvent` is unused) and completes every job, GHES
   included, through the Run Service. GHES live behavior stays unverified.
3. No telemetry, `is_background` or `background_control_*` (outside epic scope).
4. Pre-stage order/numbering (toolu runs a Node/Docker `pre` just before its
   `main` and reports it as number 0) stays as is; it belongs to pre/post
   ordering. Documented as a known difference.
5. No "Complete job" row (so no URL) when `run_job` returns a hard engine
   `Err` — before steps run, or from `run_steps`' `?` after a spawn/I-O
   error. Upstream's `StepsRunner` never throws; toolu's early-error path is
   unchanged here. Cancellation is not an error: `run_steps` returns
   `Ok(Cancelled)` and the step runs (AC-5).
6. Action preparation failures are attributed to the failing step, not to
   "Set up job": toolu resolves actions at step time (job-start prefetch
   failures never fail a job), upstream during setup.

## Architecture

### Engine (`execution`)

- `shared::job_message` deserializes `actionsEnvironment` and `billingOwnerId`.
- **Complete job step** (`job_runner/complete_step.rs`). `run_job` replaces
  the bare `evaluate_final_outputs` call: the engine mints a v4 UUID (like the
  pre-stage ids), emits `StepStarted { name: "Complete job", number }` and
  `StepMetadata { kind: "runner", action: "complete_job" }`, then:
  1. job outputs (existing `evaluate_final_outputs`; its warning now targets
     this step with upstream's text `Skip output '<name>' since it may
     contain secret.` — intentional text change),
  2. environment URL (`environment_url.rs`, below),
  3. `StepCompleted` (Success, or Failure when step 2 errored), then
     `JobCompleted { …, environment_url }`.
  The job-completed hook keeps its position after this step. `number` is one
  past the highest number the engine reported: `ExecutionContext` records the
  highest `StepStarted` number emitted by `steps_runner` and `post_drain`
  (new `note_step_number` / `next_step_number`); `run_steps`' public signature
  is unchanged. The listener reports the row like any other step: in-progress
  and completed `update_workflow_steps` entries plus a per-step log upload.
- **Environment URL** (`environment_url.rs`). Skipped when
  `actionsEnvironment` or its `url` is absent or a null token. Otherwise log
  `Evaluate and set environment url`, then evaluate like upstream
  `EvaluateEnvironmentUrl` (schema `string-runner-context-no-secrets`): the
  expression may name only `github`, `needs`, `strategy`, `matrix`, `steps`,
  `job`, `runner`, `env`, `vars` (a restricted `EvalContext` without
  `secrets`/`inputs`; `secrets.X` is an evaluation error, not a warning).
  Scalar results become strings as upstream's `TemplateEvaluator.Validate`
  converts a non-string literal (`null` → `""`, number/boolean → their
  string); a sequence/mapping or an expression error fails. Then the masker
  check from `evaluate_acquired_outputs`:
  - clean → `Evaluated environment url: <url>`, result `Some(url)`;
  - masking changes it → warning annotation on the Complete job step
    `Skip setting environment url as environment '<name>' may contain
    secret.` (absent name → `''`; text passes through the masker), result
    `None`. A literal URL containing a registered secret is suppressed too —
    deliberate deviation (upstream would still send a literal), required by
    88-S2;
  - error → `##[error]Failed to evaluate environment url` and
    `##[error]<message>`; the Complete job step concludes Failure; the job
    becomes Failure unless already Cancelled (upstream `MergeTaskResults`);
    result `None`.
  `environment_url` is reported whenever it evaluated cleanly, whatever the
  job conclusion (cancelled, failed, or later overridden by the outage
  watchdog) — upstream sends any string token.
- **Step metadata.** New `RunnerEvent::StepMetadata { step_id, kind, action,
  git_ref }`, emitted once per reported step, before its handler runs,
  mirroring `Handler.PopulateActionTelemetry` plus handler overrides:

  | Step | `type` | `action_name` | `ref` |
  | --- | --- | --- | --- |
  | `run:` | `run` | `ShellCommand.name` after `ShellCommand::resolve` (`bash`, else `sh`; `sh` in a container; `pwsh`, `python`, … for explicit shells) | — |
  | remote `uses: o/r[/p]@ref` | `node{major}` of the runtime actually run, `composite`, or `Dockerfile` | `o/r` or `o/r/p` | git ref |
  | local `uses: ./p` | same handler override | `./p` | — |
  | `uses: docker://img` | `DockerHub` | image | — |
  | Set up job (listener) | `runner` | `setup_job` | — |
  | Complete job | `runner` | `complete_job` | — |

  Pre and post stages emit the same metadata as their main step. Every
  repository/local Docker action is `Dockerfile` (upstream
  `Action.Type == Repository ? "Dockerfile" : "DockerHub"`). Composite
  children and job hooks (log-only, never `StepStarted`) emit none. Emission
  points: `steps_runner` for `run:`; `action_exec::dispatch_scoped_action`
  (node/composite/docker) and `resolve_action` (`docker://`); `post_drain` and
  the pre paths reuse the main step's values. A separate event (rejected:
  fields on the five `StepCompleted` constructors) because the kind is only
  known inside dispatch, while failures are reported from paths that never
  resolved a handler.
- **Infrastructure failures.** New `RunnerError::ActionFetch(ActionFetchError)`
  with `ActionFetchError { kind: ActionFetchKind, message: String }`
  (`RunnerError::ActionFetch` keeps `#[error("action download failed: {0}")]`
  and `ActionFetchError`'s Display is the message, so the `##[error]` step line
  is unchanged; the infrastructure annotation carries the bare message, as
  upstream uses the inner exception message) and
  `ActionFetchKind::{ResolveService, ArchiveStatus(u16), ArchiveTransport,
  ArchiveContent}`. Sites build it **where the status, cancellation and
  phase are typed values** — no message parsing. The category is a pure
  function of the kind, `ActionFetchError::infrastructure_category()`:
  `ResolveService` → `resolve_action`; `ArchiveStatus(403)` → none;
  `ArchiveStatus(_)` / `ArchiveTransport` → `error_download_action`;
  `ArchiveContent` → `invalid_action_download`. Because the category depends
  only on the final error, retries need no attempt counting:
  `ActionFetcher::ensure_action`'s existing one auth-refresh retry
  (`ARCHIVE_ATTEMPTS = 2`) now keys `is_archive_auth_error` on
  `ArchiveStatus(401 | 403)` instead of the message prefix, so a 401 then
  success reports nothing, a persistent 401 reports `error_download_action`
  and a persistent 403 none. Site table (upstream split in `ActionManager.cs`
  L1063-1110 resolve, L1660-1770 archive, L1320-1360 content;
  `LaunchHttpClient.cs` L56-67; Jev-checked):

  | Site | `ActionFetch` kind | Stays today's variant (no category) |
  | --- | --- | --- |
  | Launch download-info (`download_info.rs` `resolve_launch`) | non-success other than 422 (incl. 429), transport failure, invalid JSON → `ResolveService` | 422; cancellation |
  | Fallback REST (`resolve_fallback`) / legacy (`download_info_v1.rs`) | 429, 5xx, transport failure, invalid JSON → `ResolveService` | 401, 403, 404, 422 (missing/private repo, missing ref, no access — indistinguishable); cancellation |
  | Resolver metadata validation (`launch_info`/`fallback_info`/`legacy_info`: omitted action/repository/key, changed repository, invalid revision, `validate_archive_url`) | `ResolveService` (upstream: any resolve-phase exception) | — |
  | Missing job credential or endpoint (`… unavailable`, `ActionResolution` context errors) | — | always (configuration) |
  | Archive HTTP status (`downloader.rs`) | `ArchiveStatus(code)` for every non-success | cancellation |
  | Archive transport, client timeout, redirect limit exceeded, redirect without location | `ArchiveTransport` | — |
  | Redirect target rejected by toolu's host policy; archive URL invalid | — | always (toolu policy/config) |
  | Body-stream error during extraction | `ArchiveTransport` | — |
  | Malformed gzip/tar content (`tar entries`, `tar entry`, `entry path`, `read entry`) | `ArchiveContent` | — |
  | Tar-slip rejection | — | always (security rejection, not runner failure) |
  | Local I/O (`mkdir`, `write`, watermark, staging promote, `spawn_blocking` join) | — | always |
  | Any other existing site | — | default: unchanged |

  `extract_tarball` returns `ActionFetch(ArchiveContent)` for content errors
  and keeps `ActionDownload` for local I/O. Stream-vs-content: in
  `fetch_tarball_reader` the `bytes_stream().map(..)` closure sets a shared
  `Arc<AtomicBool>` when the HTTP stream yields an error (including the 120 s
  whole-request timeout) and returns it with the `SyncIoBridge` reader;
  `download_and_extract_action` reads it after `spawn_blocking` returns and
  turns an extraction error into `ArchiveTransport` when it is set (`GzDecoder`
  and `tar` propagate the inner `io::Error`, so the flag read afterwards is
  sound).

  Reporting: job-start prefetch failures stay WARN-only (no event, no
  category); the step-time `ensure_action` failure is reported.
  `step_errors::report_step_error` and the composite reporter
  (`composite_exec` `report_composite_step_error`, attributed to the enclosing
  step id) emit `RunnerEvent::InfrastructureError { step_id, category,
  message }` for a categorized error. The listener turns it into a step
  annotation (`level` FAILURE, `isInfrastructureIssue: true`, masked message)
  and latches the first category **only when that step's final conclusion is
  failed**; a `continue-on-error` step that turns green keeps the annotation
  but sets no category, so a successful job never reports one (upstream
  cannot reach this state because it fetches during setup). User script exit
  codes, expression errors, the user-error statuses above and the outage
  watchdog never set a category.

### Listener (`listener`)

`StepCollector` stores metadata per step id and applies it at completion,
and records infrastructure annotations/category. The forwarder latches
`JobCompleted.environment_url`. `JobOutcome` gains `environment_url`,
`billing_owner_id` (job message `billingOwnerId`; on a parse failure, the raw
acquired body's `billingOwnerId` string) and `infrastructure_failure_category`.
`report_completion` fills the request; the request is borrowed across
`retry_transient` attempts, so a retried POST is byte-identical.

**Size budget.** `execution_loop.rs` is at 499/500 code lines,
`job_lifecycle.rs` 483, `job_runner.rs` 479. `apply_outage_override` and
`LOST_CONNECTION_MESSAGE` (~35 lines) move from `execution_loop.rs` to a new
`listener/src/outage_override.rs`; completion plumbing lives in
`job_lifecycle/completion.rs` and `execution/…/job_runner/complete_step.rs`.
Target after the change: every touched file ≤ 490 code lines; watch list:
`execution_loop.rs` (499), `job_lifecycle.rs` (483), `job_runner.rs` (479),
`composite_exec.rs` (469), `download_info.rs` (396), `context.rs` (395).

### Journal (`observability`)

`From<&RunnerEvent>` maps `StepMetadata` to a new additive v1 line
`{"type":"step_metadata","step_id","kind","action","ref"}` (non-secret
action identity) and `InfrastructureError` to the existing `annotation` line
(level `error`). `job_completed` is unchanged (the URL is not journaled).
The canonical fixture is regenerated with the documented capture command.

### Wire (`wire`)

`CompleteJobRequest` gains `environment_url`, `billing_owner_id`,
`infrastructure_failure_category` (`Option<String>`, omitted when `None`).
`StepResult` switches to upstream keys and new Run Service enums; the
numeric Twirp `Status`/`Conclusion` stay for the Results Service path.

## Interfaces / Schema

```rust
// shared::job_message
pub struct ActionsEnvironment { pub name: Option<String>, pub url: Option<TemplateToken> }
// AgentJobRequestMessage
#[serde(default, rename = "actionsEnvironment")] pub actions_environment: Option<ActionsEnvironment>,
#[serde(default, rename = "billingOwnerId")]     pub billing_owner_id: Option<String>,

// shared::events::RunnerEvent
StepMetadata { step_id: String, kind: String, action: Option<String>, git_ref: Option<String> },
InfrastructureError { step_id: String, category: String, message: String },
JobCompleted { job_id, conclusion, outputs, environment_url: Option<String> },

// shared::error::RunnerError
ActionFetch(ActionFetchError),
pub struct ActionFetchError { pub kind: ActionFetchKind, pub message: String }
pub enum ActionFetchKind { ResolveService, ArchiveStatus(u16), ArchiveTransport, ArchiveContent }
impl ActionFetchError { pub fn infrastructure_category(&self) -> Option<&'static str> }

// wire::reporting
pub enum StepState { Completed }                 // serde "completed"
pub struct StepResult {
  external_id: String, number: u32, name: String,
  action_name: Option<String>,                   // omitted when None
  #[serde(rename = "ref")]  git_ref: Option<String>,
  #[serde(rename = "type")] kind: Option<String>,
  status: StepState,
  conclusion: JobConclusion,                     // "succeeded"|"failed"|"canceled"|"skipped"
  started_at, completed_at: Option<String>,
  completed_log_url: Option<String>, completed_log_lines: Option<u64>,
  annotations: Vec<Annotation>,                  // always serialized, [] when empty
}
CompleteJobRequest { …, environment_url, billing_owner_id, infrastructure_failure_category: Option<String> }
```

Wire example: `{"external_id":"…","number":2,"name":"Run actions/checkout@v4","action_name":"actions/checkout","ref":"v4","type":"node24","status":"completed","conclusion":"succeeded","started_at":"…","completed_at":"…","completed_log_url":"…","completed_log_lines":12,"annotations":[]}`.

## Failure modes and edge cases

| Input | Observable behavior |
| --- | --- |
| No `actionsEnvironment`, or `url` absent / null token | No URL log lines; `environmentUrl` omitted |
| Literal URL (incl. `""`) | Logged and sent as-is after the secret check (`""` sent) |
| Expression over `steps.<id>.outputs` | Evaluated after all main and post steps |
| Missing step output | Evaluates to `""`; `"environmentUrl":""` |
| Invalid URL text (`not a url`) | Sent unvalidated, like upstream |
| `env.X` changed via `$GITHUB_ENV` mid-job | The final value is used |
| `null` / number / boolean result | `""` / `1` / `true` |
| Sequence or mapping result; expression error; `secrets.X` reference | Error lines; Complete job Failure; job Failure (Cancelled stays); key omitted |
| Value contains a message `mask`, a secret variable value or a runtime `::add-mask::` value | Warning, key omitted, secret absent from POST, logs and journal |
| Environment name absent / contains a secret | `environment ''` / `***` in the warning |
| `billingOwnerId` absent | Key omitted |
| Launch download-info 5xx/429/transport/invalid JSON | `resolve_action`, infra annotation on the step, step and job fail |
| Launch download-info 422 | Today's user error; no category |
| Fallback REST 404 / 401 / 403 / 422 | User error; no category |
| Fallback REST 429 / 5xx | `resolve_action` |
| Launch download-info 429 | `resolve_action` |
| Resolver returned an invalid revision / omitted the action | `resolve_action` |
| Archive 401 then success after the auth refresh | No error, no category |
| Archive 5xx / 404 / persistent 401 / transport failure / mid-stream drop / redirect loop | `error_download_action` |
| Archive 403 (persistent) | No category |
| Corrupt archive | `invalid_action_download` |
| Tar-slip entry | Rejected; no category |
| Remote action fetch fails inside a composite child | Infra annotation on the enclosing step; category if that step fails |
| Local disk error while extracting | No category |
| Fetch fails on a `continue-on-error: true` step | Infra annotation; no category; job can succeed |
| Two categorized failures | First category wins |
| User script failure; outage watchdog | No category |
| `completejob` transient failure then success | Identical body re-sent; one accepted completion |
| Step reported without a handler (condition error, skipped) | No `type`/`action_name`/`ref` keys |
| Composite child annotations | Stay on the enclosing step (#82) |
| Step without annotations | `"annotations":[]` |

## Acceptance criteria

- **AC-1:** Given the sanitized github.com capture
  `crates/execution/tests/completejob_88_env_message.json` (from
  `completejob-88.yml`, `environment.url: ${{ steps.deploy.outputs.url }}`)
  and real bash, the production engine → `StepCollector` →
  `wire::net::complete_job` POST received by a local HTTP server contains
  `"environmentUrl":"<url>"`, and the "Complete job" step logs
  `Evaluated environment url: <url>`. Each boundary row above (absent, null,
  literal, `""`, missing output, invalid text, number, mapping, expression
  error, `secrets.X`) gives its listed result.
- **AC-2:** Given the captured secret-URL job message (URL from a value
  registered by `::add-mask::`) and a variant using the message's `mask`
  hint, the POST has no `environmentUrl`, the "Complete job" `StepResult`
  carries the warning annotation, and the secret string occurs nowhere in the
  POST body, the step logs or the journal.
- **AC-3:** Given real `run:` (bash and `sh` in a container where Docker is
  available), local Node with `post`, local composite, local Dockerfile,
  `docker://alpine` and a remote Node action (`actions/checkout`, network),
  each POSTed `stepResults[]` entry uses only upstream keys, string
  `status`/`conclusion`, the wire UUID as `external_id` (not the context
  name), and the `type`/`action_name`/`ref` from the table; "Set up job" and
  "Complete job" carry `runner`/`setup_job` and `runner`/`complete_job`;
  per-step annotations stay on the emitting step, including a failing `run:`
  step's `::error::` (`"conclusion":"failed"`); an annotation-free step has
  `"annotations":[]`. Docker/network rows absent on the host are reported
  unverified, never passed.
- **AC-4:** `billingOwnerId` equals the capture's `billingOwnerId` and is
  omitted when absent. Driving the production `ActionFetcher` against
  fault-injection HTTP servers (standing in for GitHub; status codes are the
  injected faults): a launch download-info 500, an archive 500, an archive
  401 on both attempts, a stream cut mid-body, and a real corrupt gzip
  archive give `resolve_action`, `error_download_action`,
  `error_download_action`, `error_download_action` and
  `invalid_action_download` respectively, each with an
  `isInfrastructureIssue` FAILURE annotation on the failing step (the
  enclosing step for a fetch inside a composite child) and that category on
  the POST; an archive 401 followed by success downloads normally with no
  category; a launch 422, fallback 404/403, persistent archive 403, a
  failing user script, an outage-watchdog failure and a `continue-on-error`
  fetch failure send no category.
- **AC-5:** When the first `completejob` POST returns 503 and the retry
  succeeds (production `retry_transient` path), both received bodies are
  byte-identical and include the URL, billing owner and step metadata; one
  completion is accepted. When the job is cancelled during a later step
  (after `deploy` wrote its output), "Complete job" still runs and the POST
  has `"conclusion":"canceled"` with the URL.
- **AC-6:** On github.com, the branch workflow `completejob-88.yml` runs the
  same jobs on a toolu runner and on GitHub-hosted `ubuntu-24.04` (official
  runner): the deployments API `environment_url` matches the reference for
  the URL job and is absent on both lanes for the secret job; both lanes end
  with a "Complete job" row whose number exceeds every other row; the
  secret-URL warning appears as a check-run annotation on both lanes; and the
  toolu job's step logs still load.
- **AC-7:** `./tools/check.sh all` passes and the documentation listed below
  describes the new payload and its verified/unverified lanes.
- **AC-8:** The journal of a real engine run contains one `step_metadata`
  line per reported step with the AC-3 values and an `annotation` line for an
  infrastructure error; the canonical fixture and reader/writer tests pass.

## Acceptance evidence

| AC | Real input | Expected | Check |
| --- | --- | --- | --- |
| AC-1 | `completejob_88_env_message.json` (capture, UUIDv5-sanitized like #99) + boundary variants derived from it (labelled capture-derived) | exact key/value, log lines, boundary table | `cargo test -p listener environment_url`; `cargo test -p execution --test environment_url_test` |
| AC-2 | `completejob_88_secret_message.json` capture; `mask`-hint variant | absence, annotation text, secret absent | `cargo test -p listener secret_environment_url` |
| AC-3 | one captured message, `completejob_88_steps_message.json`, from a `completejob-88.yml` job containing every row's step (run, local Node with post, local composite, local Dockerfile, `docker://alpine`, `actions/checkout`, a failing `::error::` step under `continue-on-error`) | exact per-step JSON objects; Docker rows unverified without Docker, the remote row unverified without network | `cargo test -p listener step_metadata` (Docker rows `#[ignore]`-gated like `docker_action_linux_test`, run with `--include-ignored` where Docker exists) |
| AC-4 | capture; fault-injection axum servers (labelled as such) and a real corrupt gzip; captured failing bash; watchdog trip | categories and annotations exact | `cargo test -p execution --test action_infrastructure_test`; `cargo test -p listener infrastructure_category`; `cargo test -p listener watchdog_trip` |
| AC-5 | recording server 503→200 (transport-body identity only; the live service half is AC-6); capture with a real `sleep` cancelled through the job token | identical bodies; cancelled body with URL | `cargo test -p listener completejob_` (both tests share the `completejob_` prefix) |
| AC-6 | `completejob-88.yml` push run, toolu runner built from the branch head | recorded API JSON in `crates/listener/tests/completejob_88_evidence.json` | `python3 scripts/test/completejob_evidence_check.py` (exits non-zero while a required lane is unverified) |
| AC-7 | workspace | green | `./tools/check.sh all` |
| AC-8 | real engine run | journal lines | `cargo test -p toolu-runner --test journal_writer_test`, `--test journal_reader_test` |

New artifacts: `.github/workflows/completejob-88.yml`, the three captures,
`completejob_88_evidence.json`, `scripts/test/completejob_evidence_check.py`.
Captures come from a branch-head toolu build with a local, uncommitted
acquire-body dump (the #68/#99 practice); the evidence JSON records the run,
revision and fixture hashes.

Platform/backend applicability: Linux github.com toolu lane and GitHub-hosted
reference lane run here; macOS live and GHES are recorded **unverified** (no
macOS runner host or GHES server here; the payload code is
platform-independent and the gate's tests run on macOS CI).

## Documentation impact

- `docs/test-coverage.md`: new #88 section (scenario → test → live result),
  and the #99 note's "no Complete job row" item resolved.
- `README.md` reporting paragraph; `docs/architecture.md` completion payload text.
- New modules: `crates/execution/src/execution/job_runner/complete_step.rs`,
  `crates/execution/src/execution/environment_url.rs`,
  `crates/listener/src/outage_override.rs`.
- READMEs: `crates/execution/src/execution/job_runner/README.md`,
  `crates/execution/src/execution/README.md`,
  `crates/execution/src/execution/actions/README.md`,
  `crates/listener/src/job_lifecycle/README.md`,
  `crates/listener/src/tests/README.md`, `crates/wire/src/reporting/README.md`,
  `crates/observability/src/journal/README.md`.
- `AGENTS.md` Key Modules (listener outage override module, journal
  `step_metadata`, completion fields).
- Intentional behavior changes: the outputs warning text becomes upstream's
  `… since it may contain secret.`; the warning now reaches GitHub on the
  Complete job step.

## Issue criteria reconciliation

| Issue item | Covered by |
| --- | --- |
| URL evaluated at job end and sent (88-S1, incl. empty/absent/invalid) | AC-1, AC-6 |
| Secret-bearing URL suppressed with masked warning (88-S2) | AC-2, AC-6 |
| StepResult action name/ref/type + per-step annotations; UUID vs context name (88-S3) | AC-3, AC-8 |
| `billingOwnerId` echoed; category for runner-side failures (88-S4) | AC-4 |
| Completion transport failure / cancellation / GHES (88-S5) | AC-5, AC-6; GHES is Non-Goal 2, recorded unverified |
| Real-data test, gate, evidence mapping | Acceptance evidence, AC-6, AC-7 |

## Open Questions

None blocking. Exact post-row numbers differ from the reference (upstream
assigns record orders, e.g. 54); AC-6 asserts only that "Complete job" is last.

## Review log

Adversarial spec review (independent reviewer, four rounds): rev 1 →
category premise wrong (three `ActionManager` categories), secrets context,
captures not real, AC-5 command broken; rev 2 → typed status classification,
fallback/legacy user-error statuses, stream-vs-content; rev 3 → auth-retry
keyed on message text, composite path, uncovered sites; rev 4 → Display
prefix (fixed in rev 5). Final verdict: no blockers; should-fix applied.
