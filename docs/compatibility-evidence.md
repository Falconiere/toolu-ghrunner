# Compatibility evidence (epic #67, issue #103)

This document explains how toolu-runner's compatibility with the official
[actions/runner](https://github.com/actions/runner) is evidenced, and where that
evidence stops. The per-criterion record is
[compatibility-matrix.md](compatibility-matrix.md). That view is generated from
[compatibility-matrix.json](compatibility-matrix.json) and validated in CI by
`scripts/test/compat_matrix_check.py`. The matrix JSON, its generated
Markdown view, the reference report and the captures are generated data files,
not code, so the 500-code-line ceiling does not apply to them.
`scripts/guardrails/` checks only the `crates/*` packages listed in
`guardrails.workspace.json`, so `docs/` is outside its scope. The captures live
under `crates/execution/tests/`, which the ceiling exempts.

**A green repository gate is not drop-in compatibility.** Only lanes marked
`passed` with evidence count. Everything else is an explicit gap.

## Lanes and statuses

Every matrix row records all seven lanes:

| Lane | What it means | `passed` evidence |
| --- | --- | --- |
| `hermetic` | `cargo nextest run --workspace` (plus the doctests) on the ubuntu CI leg, and the Python evidence-tool tests in the "Compatibility evidence (#103)" CI step | `gate`: at least one mapped, non-`#[ignore]` test |
| `macos` | The same Rust suite on the `ci-macos` job | `gate`: at least one mapped, non-ignored Rust test whose file is not `#![cfg(target_os = "linux")]` |
| `docker` | A real Linux Docker daemon | `ci-docker` (a mapped test run by `ci.yml`'s Docker step), a recorded explicit run (`doc:` or a `pr:` whose body records it), or a live run (`run:`) |
| `github-com` | A real job acquired from GitHub.com and executed by toolu | `run:<actions run URL>` or `pr:<PR URL>` whose body records the observation |
| `reference` | The same workflow/action revisions compared against the official runner | `run:` (paired run) or `report:docs/compat-reference-103.json` |
| `ghes` | GitHub Enterprise Server | none recorded; no GHES instance has been available; out of scope (excluded) |
| `ui-service` | A GitHub-owned UI or backend result: annotations, summaries, the cache/artifact/OIDC backends, recorded job/step results | `run:` / `pr:` |

Statuses:

- `passed` needs `evidence`.
- `unverified` needs a reason. Missing credentials, tools, hosts or reference
  runners, skipped or ignored tests, and recorded differences all land here.
- `not-applicable` needs a reason explaining why the lane cannot apply.
- `pending` needs a reason. It is used only for child issues whose evidence is
  not recorded yet and must link the issue. No row uses it today.

The checker enforces the mechanical parts:

- Every mapped test is a real `#[test]`/`#[tokio::test]` fn or `unittest`
  method.
- An ignored test never backs `gate`.
- `run:`/`pr:` URLs belong to this repository.
- `doc:` anchors resolve.
- A row cannot pass the `reference` lane by citing a report that acknowledges a
  difference for it, or that report's run. A fresh report can back it while the
  old one stays unedited.
- The `reference` lane takes only `run:` and `report:` evidence.
- A row whose issue is not in the matrix `issues` is rejected, except epic `67/`
  rows.
- Each issue has exactly one row per AC/scenario id.

IDs are extracted deterministically from the issue bodies:

- `AC-<n>` is the n-th checkbox under `## Acceptance criteria`.
- Scenario ids come from the first column of the scenario table.

Run `compat_matrix_check.py --extract-ids` to print them, and `--remote` to
re-compare ids, issue states and run links with GitHub.

The `103/R-*` rows cover the cross-feature regression surfaces: cache,
artifacts, OIDC, command files, masking, acquire, renew, complete,
cancellation and temp cleanup. The `67/X-*` rows cover the epic's six cross-feature workflows.

## Capture and sanitization

To capture, build with the non-default `capture` feature:

```sh
cargo build -p toolu-runner --features capture
```

With that feature, `listener::capture::write_acquired` writes every acquired
job body to `<data_dir>/_diag/captures/<UTC>-<plan_id>.json`. The file is
`0600` inside a `0700` directory. A failed write is WARN-logged and never
affects the job. Release builds never enable the feature.

A raw capture contains the job's tokens and secrets. Never commit it. Sanitize
it first:

```sh
python3 scripts/test/sanitize_capture.py RAW.json crates/execution/tests/<name>.json --namespace <name>
python3 scripts/test/sanitize_capture.py --scan crates/execution/tests/<name>.json
```

How the sanitizer treats the message:

- **What it replaces.** Secret `variables`, `mask` hints, endpoint
  `authorization.parameters`, and any GitHub-token- or JWT-shaped string.
- **Placeholders.** Each distinct secret becomes a version-5 UUID derived from
  the namespace and the JSON path where it was first seen, never from the
  value, so no hash of a secret is committed.
- **Endpoint authorization.** Endpoint `authorization.parameters` values (and
  every other occurrence of them) become the literal `redacted`, the #99
  convention. A UUID under an `AccessToken` key reads as a live API key to
  gitleaks.
- **Consistency.** Every occurrence of a secret, including inside scripts, is
  replaced by the same placeholder, so equality relations survive.
- **Run identifiers.** `plan.planId` and `jobId` are not credentials, but they
  tie a fixture to its live run (they are embedded in `GenerateIdTokenUrl`, for
  example). They become UUIDv5 placeholders at every occurrence, so the URL and
  the body fields stay consistent and the replay remains valid.
- **What it keeps.** Keys, key order, value types, template-token shapes, step
  UUIDs, `contextName`s and service URLs are untouched.
- **Refusal.** The output is re-scanned, and the tool exits non-zero if a
  credential-shaped string or an original secret survives.

Committed captures from the #103 reference run:

| Fixture | Source job | Raw SHA-256 |
| --- | --- | --- |
| `crates/execution/tests/compat_103_contexts_message.json` | `probe-toolu / contexts` | `b2563d964ca71587b71d600641b5963a631b1e7f4c59c5543d12aae4b74c95c7` |
| `crates/execution/tests/compat_103_docker_message.json` | `probe-toolu / docker` | `c441f7811ea694d7a51bab2d206fee23700146a35012cd669ec2520f84db07dd` |

Both captures replay through the production engine:

- `replay_mutation_test.rs` runs in the default gate (#103 S1). The
  unmodified job passes. Dropping inputs, matrix, workflow/job env,
  `defaults.run` or `jobOutputs` makes it fail at the named step, or lose its
  outputs.
- `compat_probe_replay_test.rs` uses real Docker and runs in CI's Docker step
  (#103 S2). The container step's Alpine-only output and marker are asserted.
  Replacing it with an `echo` of a `docker://` string, or emptying `who`,
  fails the verify step.

Captures made before #103 used one-off instrumentation; see each fixture's
own provenance note, for example
`crates/toolu-runner/tests/fixtures/job_container_message.md`.

## Reference baseline provenance

The paired run is
[38301583727](https://github.com/Falconiere/toolu-ghrunner/actions/runs/38301583727).
It ran `.github/workflows/compat-reference-103.yml` on 2026-10-12, calling
the reusable `compat-reference-103-probe.yml` for two variants:

- `probe` must succeed.
- `negative` passes an empty `who` and must fail at the input assertions.

Both variants ran on both lanes.

| | Official lane | toolu lane |
| --- | --- | --- |
| Runner | unmodified actions/runner **2.337.0** (release commit `397b032cbf865e9c3ddfab89d533ec19325e1273`), `actions-runner-linux-x64-2.337.0.tar.gz` SHA-256 `70920811a4f8ad4328818682bca5c6469c1c942fab52448868071d0063816613` (matches the release notes), `--disableupdate` | toolu-runner 0.10.0 from commit `a0afa0b`, built with `cargo build -p toolu-runner --features capture` (debug), binary SHA-256 `6acafaf634e11aecf2aef0c3badee8b1e74c474251d7f21968115be19513d0d6`, always-online `run` loop, forwarder services mode |
| Labels | `self-hosted, Linux, X64, toolu-103-official` | `self-hosted, Linux, X64, toolu-103-toolu` |
| Host | one Linux x86_64 host, Docker Engine 29.1.3; both runners ran as root in isolated homes against the same daemon | same |

Pins:

- Image: `alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc`
  (3.20.10).
- `actions/cache` `55cc8345863c7cc4c66a329aec7e433d2d1c52a9` (v6.1.0).
- `actions/upload-artifact` `cf430e030ddbb5b0abf93d22962f4752f3646cd9`
  (v7.0.2).
- `actions/download-artifact` `9000827ccba6bdab643e8b6fd33ac0654aef8333`
  (v8.0.2).

`scripts/test/reference_compare.py` pairs jobs by variant, job, OS and arch,
normalizing only the lane token in job names. It compares:

- job conclusions and the ordered step (name, conclusion) list;
- each lane's uploaded `toJSON(needs)` observations, including JSON types, so
  `true` differs from `1` and a missing key from `null`;
- exact payload artifact bytes, read back through the GitHub API (this checks the upload; the in-job `download-artifact` step is compared only through the step lists);
- each variant's expected outcome;
- leftover probe containers on each Linux VM, from the listing its host job uploads.

A duplicate job, or missing or empty `run`, `provenance` or `results`, is an
error. Every report must cover Linux X64, Linux ARM64 and macOS ARM64; macOS has
no Docker jobs, and one that ran there is rejected.

This run's report is [compat-reference-103.json](compat-reference-103.json). CI
re-verifies it offline with `--verify`, which also re-hashes the committed
captures. It is the one report allowed to name no platform, pinned by run id.
The recorded run came from a temporary branch `push` trigger, removed before
merge.

### Paired workflow

[`compat-paired.yml`](../.github/workflows/compat-paired.yml) is the way to run a
new comparison. It is dispatch-only and has not been dispatched yet, so two
runners side by side on a hosted VM remains unproven.

Each of `ubuntu-latest`, `ubuntu-24.04-arm` and `macos-15` verifies the official
v2.338.2 archive digest, builds toolu, registers both runners by JIT under
per-run labels, and serves them until that platform's probe jobs complete.
Runners are unregistered on every exit. A missing `TOOLU_RUNNER_LIVE_TOKEN` fails
the run as `UNVERIFIED`.

`negative` is meant to fail, so a good run still concludes `failure`. Compare with:

```sh
python3 scripts/test/reference_compare.py <run_id> --out docs/<report>.json \
  --provenance <file> --capture <fixture> [--acknowledge <file>]
```

`--out` is required and must not exist, so a comparison cannot overwrite a
recorded report.
Acknowledgements name platform scopes such as `probe-Linux-X64`, so one platform's
acknowledgement cannot excuse another's difference. `compat-reference-103.yml`
stays for the record; the comparator no longer reads runs in its job layout.

**Not covered:**

- macOS and ARM64 pairs: the recorded run has no host for them.
  `compat-paired.yml` covers them but has not been dispatched, so they stay
  `unverified`.
- GHES: out of scope and a matrix exclusion. The V1 code exists but has never run
  on a real server, and `live-ghes.yml` cannot test it (its harness registers at
  github.com; see [known bugs](known-bugs.md#verification-limits)).
- Accelerated services mode.
- Live fault injection on the control plane (outage, cancel races, duplicate
  delivery).
- Live UI probes for annotations, summaries and deployment URLs.

These remain `unverified` in the matrix.

## Recorded differences

The paired run found two real differences. Per #103's spec, a difference is
recorded rather than fixed in the evidence PR. It stays in the report under
`acknowledged`, and it keeps the named matrix rows' `reference` lane
unverified. Each acknowledgement pins the exact step lists it excuses on both lanes, so
a new divergence in the same job is not covered. `--verify` recomputes every
difference from those recorded step lists rather than trusting stored
`equal` flags. An acknowledgement that no longer matches an observed
difference fails `--verify`, so a fix forces the record to be updated.

1. **`forwarder-cache-service-v2`** (rows `103/S4`, `103/R-cache`).
   - **Cause.** toolu never exports `ACTIONS_CACHE_SERVICE_V2` or
     `ACTIONS_CACHE_MODE`. `execution::service_endpoints` reads a job variable
     named `ACTIONS_CACHE_SERVICE_V2`. GitHub sends
     `actions_uses_cache_service_v2 = true` and `actions_cache_mode = write`
     (visible in the committed captures).
   - **Upstream mapping.** actions/runner `cab9d1c` maps them in
     `NodeScriptActionHandler.cs` / `ContainerActionHandler.cs`, and prints
     `Cache mode: write` in Set up job (`JobExtension.cs`).
   - **Effect.** `actions/cache` falls back to the retired v1 host
     `artifactcache.actions.githubusercontent.com`. `Save cache` logs
     `Failed to save: … Our services aren't available right now` yet succeeds,
     so no cache entry exists. The dependent `Restore cache` fails with
     `Cache service responded with 400`.
   - **Knock-on effect.** On toolu, the consumer job's `Download artifact`
     step was skipped after the restore failed, so `download-artifact` v8 is
     observed only on the official lane (row `103/R-artifacts` stays
     unverified).
   - **Test gap.** `gh_compat_forwarder.rs::forward_env_cache_v2_only_when_true`
     sets `cache_service_v2` directly, so the variable-name lookup in
     `service_endpoints` is never exercised. It also expects `true` where
     upstream sets `True`, and it never checks `ACTIONS_CACHE_MODE`.
2. **`docker-pull-step-reporting`** (rows `103/S3`, `75/75-S1`). For a
   `docker://` step the official runner reports an extra job-setup step,
   `Pull alpine@sha256:…`; toolu does not. All shared steps have identical
   names and conclusions, container outputs and marker bytes match, and no
   container is left behind.

The causes above describe the runner at the time of that run. #253 has since
mapped the cache variables per step type, and #255 reports Docker `Pull` setup
steps. The recorded report predates both, so the named rows' `reference` lane
stays `unverified` until a new paired run compares them, and the matrix does not
yet claim cache parity or identical Docker step reporting.

## Approved scope exclusions

These are the epic's exclusions. They are not evidenced and not claimed:

- Windows (`cmd`/`powershell`, zipball actions).
- Container hooks (k8s mode).
- Immutable OCI-packaged actions.
- `background:` steps.
- The step debugger (DAP).
- `snapshot:` (GitHub-hosted only).
- Attestation artifact file commands.
- Runner telemetry.
- AuthMigration.

#94 merged in PR #171 and #97 in PR #173. Their rows record what tests prove at
the delivered revision; their live, paired and GHES lanes stay `unverified` or
`not-applicable`.
