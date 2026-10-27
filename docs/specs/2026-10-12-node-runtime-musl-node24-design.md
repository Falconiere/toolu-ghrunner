# Node runtime: musl/Alpine builds, node24 migration flags, offline preseed — Design

> Historical implementation record. Current behavior is documented in
> [architecture](../architecture.md); acceptance status and remaining gaps are
> tracked in [test coverage](../test-coverage.md) and the
> [compatibility matrix](../compatibility-matrix.md).

**Date:** 2026-10-12   **Status:** Approved   **Author:** Claude (epic #67 worker)   **Topic:** issue #95 — run Node actions on a runtime that starts in their execution environment and pick node20/node24 exactly as upstream

## Problem

`crates/execution/src/node/runtime.rs` downloads glibc/darwin Node from
`nodejs.org/dist` only. A musl host (Alpine) gets a binary that cannot start,
and an Alpine **job container** gets the host's glibc binary bind-mounted at
`/github/node`, which cannot start either. An unknown CPU architecture is
silently mapped to `x64`. There is no offline/pre-seeded runtime: an airgapped
host cannot run any Node action.

`actions/manifest.rs::parse_runs_using` redirects `node12`/`node16` to `node20`
and otherwise uses the declared major. Upstream (`actions/runner` `cab9d1c`,
v2.337.0, `HandlerFactory.cs:60-145`, `NodeUtil.cs`) additionally decides
node20 → node24 from server feature flags and two env switches. The captured
github.com job messages already committed in this repo
(`crates/execution/tests/job_outputs_message.json`,
`completejob_88_steps_message.json`,
`crates/toolu-runner/tests/fixtures/job_container_message.json`) carry
`actions.runner.requirenode24 = "true"`: **today GitHub's official runner runs
every node20 action on Node 24, while toolu runs it on Node 20** — a silent
divergence.

## Non-Goals

1. Linux ARM32 (`actions_runner_deprecate_linux_arm32` /
   `actions_runner_kill_linux_arm32`): toolu ships no armv7 build. Such a host
   gets the unsupported-architecture error (AC-3); the ARM32 phases and the
   "stuck on Node.js 20 on ARM32" annotation are not applicable.
2. Container hooks (k8s mode) — out of epic scope.
3. Checksum/signature verification of downloaded runtimes (upstream verifies
   nothing at job time either; the image build already checks SHASUMS256).
4. A `config.toml` key for the preseed directory (env only; see Architecture).
5. Windows.
6. Musl builds for arm64: `actions/alpine_nodejs` publishes x64 only; arm64
   musl fails clearly, exactly as upstream does for Alpine containers.
7. Upstream's job telemetry for upgraded/deprecated actions (only the
   user-visible annotations are in scope).

## Architecture

Three independent pieces meet at the single runtime call site,
`execution::node_stage::run_node_stage`, which runs for every
`pre`/`main`/`post` stage of every top-level and composite-nested Node action
(upstream likewise creates the handler — and decides the version — per stage).

### 1. Version selection (pure port of upstream)

New `execution::node::migration`:

- `NodeMigrationFlags::from_variables(&HashMap<String, VariableValue>)` reads
  the job message variables case-insensitively (upstream `Variables` uses
  `OrdinalIgnoreCase`): `actions.runner.usenode24bydefault`,
  `actions.runner.requirenode24`, `actions.runner.warnonnode20` with .NET
  `bool.TryParse` semantics (trimmed, case-insensitive `true`/`false`;
  anything else ⇒ unset ⇒ `false`), plus `actions_runner_node24_default_date`
  / `actions_runner_node20_removal_date` (empty ⇒ upstream fallbacks
  `June 16th, 2026` / `September 23rd, 2026`). Stored on `ExecutionContext` at
  job start (next to `ProcessTracking::for_job`).
- `select_node_version(declared_major, &flags, workflow_env, system_env) ->
  NodeSelection` ports `DetermineActionsNodeVersion` verbatim:
  - declared 24 ⇒ 24, no messages (ARM32 N/A).
  - declared 20 (incl. the 12/16 redirect) ⇒
    1. `requirenode24` ⇒ 24;
    2. `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` and
       `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION` both true **from the same
       source** (both in workflow env, or both in system env) ⇒ the default
       (24 if `usenode24bydefault` else 20) plus upstream's warning
       `Both FORCE_JAVASCRIPT_ACTIONS_TO_NODE24 and
       ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION environment variables are set
       to true in the {workflow|system} environment. This is likely a
       configuration error. Using the default Node version: {node20|node24}.`
       (workflow checked first, as upstream);
    3. `usenode24bydefault` ⇒ 20 if allow-unsecure is true else 24;
    4. force true ⇒ 24; else 20.
  - Per-variable truth: if the key is present in the workflow env its
    `StringUtil.ConvertToBoolean` value wins (`1`/`true`/`$true` ⇒ true,
    case-insensitive; empty/other ⇒ false); otherwise the system value is
    used. "From system" means the process variable is non-empty.
  - Messages for declared 20: the config warning (above); when
    `usenode24bydefault && !requirenode24 && final == 24`, the info line
    `Node 20 is being deprecated. This workflow is running with Node 24 by
    default. If you need to temporarily use Node 20, you can set the
    ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true environment variable. For
    more information see:
    https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/`.
  - Tracking for declared 20 with a non-empty action name: final 24 ⇒
    "upgraded"; else `warnonnode20` ⇒ "deprecated".
- **Workflow env** = the stage's built env map (`build_node_env`: env context
  — workflow/job/step `env` + `GITHUB_ENV` — plus `INPUT_*`/`STATE_*`/runner
  vars; it is delta-only, so it never contains the runner process env).
  Keys are case-sensitive (upstream's `VarUtil.EnvironmentVariableKeyComparer`
  is ordinal on Linux/macOS). **System env** = the runner process env, read
  through `execution::config::var`.
- Action name = upstream `GetActionName` over the step's reference:
  `path` part = `path` if `name` empty else `/{path}` (only when `path`
  non-empty); `{name}{path}` plus `@{ref}` when `ref` non-empty; empty ⇒ none.
- Emission: the config warning is a step-level warning (`##[warning]…` log
  line + `RunnerEvent::Annotation{level: Warning}` on the stage's log step);
  the info line is a plain log line. Tracking sets live on
  `ExecutionContext` (deduplicated). The "Complete job" row
  (`job_runner/complete_step.rs`), after the environment URL, emits upstream's
  two warnings, actions sorted case-insensitively, joined with `", "`:
  - deprecated: `Node.js 20 actions are deprecated. The following actions are
    running on Node.js 20 and may not work as expected: {list}. Actions will be
    forced to run with Node.js 24 by default starting {node24DefaultDate}.
    Node.js 20 will be removed from the runner on {node20RemovalDate}. Please
    check if updated versions of these actions are available that support
    Node.js 24. To opt into Node.js 24 now, set the
    FORCE_JAVASCRIPT_ACTIONS_TO_NODE24=true environment variable on the runner
    or in your workflow file. Once Node.js 24 becomes the default, you can
    temporarily opt out by setting ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true.
    For more information see: {url}`
  - upgraded: `Node.js 20 is deprecated. The following actions target Node.js
    20 but are being forced to run on Node.js 24: {list}. For more information
    see: {url}`
  Both go through `CompleteStep::mask` (action names are user data).
- `parse_runs_using` matches upstream `ActionManifestManager`: `node12`,
  `node16`, `node20`, `node24` case-insensitively (12/16 ⇒ 20); any other
  `node*` value fails with upstream's text `'using: {value}' is not supported,
  use 'docker', 'node12', 'node16', 'node20' or 'node24' instead.`

### 2. Runtime target (host vs. job container)

New `execution::node::platform`:

- `NodeTarget { os: Linux|Darwin, arch: X64|Arm64, libc: Glibc|Musl }`.
- Host: `std::env::consts` OS/arch; anything other than linux/macos ×
  x86_64/aarch64 ⇒ `RunnerError::NodeRuntime("Node.js actions are not
  supported on {OS} {ARCH}")` (GitHub spellings via `shared::platform`).
  Linux libc = `detect_libc(Path::new("/"))`: a glibc loader
  (`lib64/ld-linux-x86-64.so.2`, `lib/ld-linux-aarch64.so.1`,
  `lib/x86_64-linux-gnu/ld-linux-x86-64.so.2`,
  `lib/aarch64-linux-gnu/ld-linux-aarch64.so.1`) ⇒ Glibc; else a
  `lib/ld-musl-*.so.1` ⇒ Musl; else Glibc (startup validation then decides).
  A glibc host that merely has the Debian `musl` package stays Glibc.
- Job container (Linux only, already enforced): target = Linux, host arch,
  libc from upstream's probe — `sh -c "cat /etc/*release | grep ^ID"` exec'd
  in the container once per job (cached on `JobContainer`); any output line
  whose lowercase contains `alpine` ⇒ Musl; non-zero exit or no match ⇒ Glibc
  (upstream: "best effort").
- Musl on a non-x64 arch fails before execution: container ⇒ upstream text
  `JavaScript Actions in Alpine containers are only supported on x64 Linux
  runners. Detected Linux Arm64` (upstream's enum spellings); host ⇒ `JavaScript Actions on musl (Alpine)
  hosts are only supported on x64 Linux runners. Detected Linux Arm64`.

### 3. Acquisition: preseed, cache, download

`ensure_node_runtime(client, config, major, target) -> PathBuf`:

- **Preseed (authoritative).** `RunnerConfig.node_dir: Option<PathBuf>`
  (new field; the bin fills it from `TOOLU_RUNNER_NODE_DIR` in both `run` —
  per loop iteration — and `boot`). Layout = upstream's `externals/`:
  `<dir>/node20/bin/node`, `<dir>/node24/bin/node`,
  `<dir>/node20_alpine/bin/node`, `<dir>/node24_alpine/bin/node` (an official
  runner's `externals/` folder works as-is). When set, toolu **never
  downloads**: a missing binary fails with `Node.js runtime node24_alpine not
  found in TOOLU_RUNNER_NODE_DIR: expected <path>`. A host-target preseed
  binary is validated on every use (below). Job containers get
  `<dir>` bind-mounted read-only at `/github/externals` so the path translates.
- **Cache.** Otherwise `{data_dir}/node/{version}` (nodejs.org build for the
  host OS — unchanged naming so the image seeds and existing test seeding keep
  working) or `{data_dir}/node/{version}-alpine` (musl). A present binary is
  trusted (it was committed atomically after validation).
- **Download.** nodejs.org:
  `https://nodejs.org/dist/v{v}/node-v{v}-{linux|darwin}-{x64|arm64}.tar.gz`
  (strip the top-level dir); musl:
  `https://github.com/actions/alpine_nodejs/releases/download/v{v}/node-v{v}-alpine-x64.tar.gz`
  (rooted at `./`, nothing to strip; leading `.` components are always
  dropped). The existing stream → `spawn_blocking` extract → staging →
  `promote_staging` rename is kept. Through the caller's `reqwest::Client`
  (`job.http`) — no new client builder, so #94's proxy/CA settings apply.
- **Startup validation.** When the target is host-runnable (target libc == host
  libc and OS/arch == host), run `<node> --version` (30 s bound, no shell,
  `kill_on_drop`) **inside staging before promote**: it must exit 0 and print
  `v{major}.`. Failure removes staging and fails with `{cache dir} from {url}
  was not installed: Node.js runtime {binary} failed to start on this host:
  {cause}` (or `… reported version "…", expected v{major}.x`) — the cache
  never commits a runtime that cannot start. Container-only targets (e.g. Alpine runtime on a glibc
  host) are not executed on the host; the container run proves them.
- **Pins** match upstream `externals.sh`: node20 ⇒ `20.20.2`, node24 ⇒
  `24.21.0` (`actions/alpine_nodejs` has no `20.18.3`). `Dockerfile` and
  `Dockerfile.macos` seed versions move in lockstep.

`run_node_stage` order becomes: resolve script → build env → select version
(emit warning/info, record tracking) → resolve target (host or container) →
ensure runtime → execute. Every failure before execution fails the stage
(the action never starts); the error reaches the step log and the job result
like any other stage error today.

`node/runtime.rs` (345 lines) is split so no file nears the 500-line ceiling:
`runtime.rs` (ensure/orchestration), `download.rs` (fetch/extract/staging),
`platform.rs`, `migration.rs`, `validate.rs`; `node/README.md` gains a row
each.

## Interfaces / Schema

```rust
// execution::node::platform
pub enum NodeOs { Linux, Darwin }
pub enum NodeArch { X64, Arm64 }
pub enum Libc { Glibc, Musl }
pub struct NodeTarget { pub os: NodeOs, pub arch: NodeArch, pub libc: Libc }
pub fn host_target() -> Result<NodeTarget, RunnerError>;
pub fn detect_libc(root: &Path) -> Libc;
pub fn container_target(os_release_lines: Option<&str>) -> Result<NodeTarget, RunnerError>;

// execution::node::migration
pub struct NodeMigrationFlags { pub use_node24_by_default: bool, pub require_node24: bool,
  pub warn_on_node20: bool, pub node24_default_date: String, pub node20_removal_date: String }
pub struct SystemNodeEnv { pub force_node24: Option<String>, pub allow_unsecure: Option<String> }
pub enum Tracking { UpgradedToNode24, DeprecatedNode20 }
pub struct NodeSelection { pub major: u8, pub warning: Option<String>,
  pub info: Option<String>, pub tracking: Option<Tracking> }
pub fn select_node_version(declared: u8, flags: &NodeMigrationFlags,
  workflow_env: &HashMap<String, String>, system: &SystemNodeEnv) -> NodeSelection;
pub fn action_name(reference: &ActionStepDefinitionReference) -> Option<String>;

// execution::node::runtime
pub async fn ensure_node_runtime(client: &reqwest::Client, config: &RunnerConfig,
  major: u8, target: NodeTarget) -> Result<PathBuf, RunnerError>;

// shared::RunnerConfig
pub node_dir: Option<PathBuf>, // TOOLU_RUNNER_NODE_DIR

// docker::JobContainer
pub(crate) async fn node_target(&self) -> Result<NodeTarget, RunnerError>; // cached probe
```

Env: `TOOLU_RUNNER_NODE_DIR` (bin `config.rs`); `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24`,
`ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION` (execution `config.rs`, system side).

## Failure modes and edge cases

| Input | Behavior |
| --- | --- |
| Flag variable absent / `"yes"` / `" TRUE "` | absent/`yes` ⇒ false; `" TRUE "` ⇒ true (TryParse trims) |
| Env switch `"1"`, `"$true"`, `"TRUE"` | true; `""`, `"yes"` ⇒ false; workflow key present with `""` ⇒ false even if system is true |
| Both switches true, one in workflow, one in system | not "same source" ⇒ phase rules apply, no warning |
| `using: Node20` | accepted as node20 |
| `using: node22` | manifest error (upstream text); step fails |
| Musl host x64 | `-alpine` runtime downloaded, validated, used |
| Musl host arm64 / Alpine container on arm64 | clear error before execution |
| Alpine+gcompat host (glibc loader shim) | detected Glibc; glibc Node fails validation ⇒ clear error naming the start failure |
| Container without `sh` (distroless) | probe exits non-zero ⇒ Glibc (upstream) |
| Preseed dir set, runtime missing | error naming the expected path; no network |
| Preseed binary not executable / prints wrong major / not Node | validation error naming the binary and the cause |
| Preseed symlinks escaping `<dir>` | host: fine; container: may dangle — documented (copy real files) |
| Download HTTP error / corrupt tarball / interrupted stream | staging removed, cache dir untouched, stage fails |
| Downloaded binary cannot start on host | staging removed, nothing committed, clear error |
| Two processes install the same runtime | existing `promote_staging` race handling; one tree wins |
| Unsupported host OS/arch (armv7, FreeBSD) | `Node.js actions are not supported on …` |

## Acceptance criteria

- **AC-1:** Every one of the 27 `[InlineData]` rows of upstream's own oracle,
  `src/Test/L0/Util/NodeUtilL0.cs` @`cab9d1c` (`TestNodeVersionLogic` 12 rows,
  `TestNodeVersionLogicWithWorkflowEnvironment` 15 rows), committed verbatim as
  a JSON fixture with provenance, yields the same version and warning presence
  from `select_node_version`; additionally node12/16/24 declarations and the
  exact warning/info strings match upstream's source text, and the flags parse
  from the committed captured github.com message.
- **AC-2:** Given the committed captured github.com job message
  (`requirenode24 = "true"`) and a real local node20 action, the production
  `Runner::execute_job` path runs the action with the node24 runtime (its
  `process.execPath` is under `node/24.21.0`) and the "Complete job" row
  carries upstream's "forced to run on Node.js 24" warning naming the action;
  with the same message's flags set to `usenode24bydefault=true`,
  `requirenode24=false` and step env `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION:
  true`, it runs on node20, emits no upgrade warning, and (the captured
  `warnonnode20 = "true"`) the "Complete job" row carries upstream's
  "Node.js 20 actions are deprecated" warning naming the action.
- **AC-3:** Host target detection returns Glibc on this glibc host and
  Darwin/Arm64 on the `ci-macos` leg, Musl for a real exported Alpine root
  filesystem, Glibc for an exported Debian root that also has the `musl`
  package's `ld-musl` installed; an unsupported OS/arch pair or musl+arm64
  yields the documented error before any download.
- **AC-4:** A real `actions/alpine_nodejs` runtime runs a real node20 action
  inside an `alpine` job container through the production path
  (`process.report` shows no glibc runtime, version 24 under the captured
  flags), and a glibc runtime runs it inside an `ubuntu` job container.
- **AC-5:** With `node_dir` pointing at a directory holding a real Node under
  `node{M}/bin/node`, `ensure_node_runtime` returns it without any network
  access; a missing `node{M}` fails naming the expected path; a non-Node or
  wrong-major binary fails validation.
- **AC-6:** Served over a real local HTTP server, a tarball holding the real
  host Node binary installs and validates; a tarball whose `bin/node` is a
  real non-Node system binary (`/bin/echo`), or — in the network lane — the
  real `actions/alpine_nodejs` binary on a glibc host without a musl loader
  (cannot start), is never committed to the cache dir; a corrupt/truncated body leaves no cache
  dir; concurrent installs still commit exactly one tree; the musl tarball
  layout (`./bin/node`) extracts to `bin/node`.
- **AC-7:** `using: node22` fails with upstream's message; `using: Node20`
  parses as node20.
- **AC-8:** `./tools/check.sh all` is green with no suppressions.

## Acceptance evidence

| AC | Real input | Expected | Check |
| --- | --- | --- | --- |
| AC-1 | `crates/execution/tests/node_migration_upstream_l0.json` — upstream `NodeUtilL0.cs` @`cab9d1c` rows verbatim (source path + line per row); captured `job_outputs_message.json` variables | same version + warning presence per row; exact strings |  `cargo test -p execution node::migration` |
| AC-2 | `crates/execution/tests/job_outputs_message.json` variables (captured github.com) + local node20 action; real host Node copied into the cache dir | execPath under `node/24.21.0`; Complete-job upgrade warning text; flipped flags ⇒ node20 + deprecation warning | `cargo test -p execution --test node_migration_test` |
| AC-3 | `docker export` of pinned `alpine` and `debian` images (+ debian with `musl` pkg) as roots; this Linux host; the `ci-macos` host | Musl / Glibc / Glibc / host Glibc / Darwin Arm64; error strings | `cargo test -p execution node::platform` (both CI legs); ignored Docker lane for the exported roots |
| AC-4 | Pinned `alpine` and `ubuntu` images, real downloads, captured flags message | action output proves runtime/libc; job Success | `cargo test -p execution --test node_container_test -- --ignored` (CI Docker step) |
| AC-5 | Real host `node` placed in a temp preseed dir; client pointed at an unroutable base | path returned / missing-path error / validation error | `cargo test -p execution node::runtime` |
| AC-6 | Local HTTP server serving tarballs built from the real host `node` (nodejs.org `node-v…/` layout and alpine `./` layout), from real `/bin/echo`, and a truncated real tarball body | committed + validated / no cache dir on failure; one tree under concurrency | `cargo test -p execution node::` |
| AC-6 (real) | Real nodejs.org + alpine_nodejs downloads | glibc validates on host; alpine `bin/node` host-targeted commits only if it starts (refused on a glibc host without `ld-musl`, e.g. CI ubuntu; starts on a host with Debian's `musl` package); committed for the container target | ignored network lane |
| AC-7 | action.yml with `using: node22` / `Node20` | upstream error / node20 | `cargo test -p execution actions::manifest` |
| AC-8 | whole workspace | exit 0 | `./tools/check.sh all` |

Applicability: Linux x64 glibc host + Docker verified locally and in CI; macOS
arm64 host selection/validation runs in the `ci-macos` leg (host lane only);
Linux arm64 and real musl hosts are covered by the exported-root detection
test and the error path, not a live arm64/Alpine host — recorded as
unverified in `docs/test-coverage.md`. GitHub.com vs GHES: flags arrive as job
variables on both; GHES not exercised live (unverified).

## Documentation impact

- `README.md`: Node runtime section — musl/Alpine support, node24 migration
  behavior, `TOOLU_RUNNER_NODE_DIR`, new pins.
- `docs/test-coverage.md`: new "Node runtime (#95)" section mapping AC/S rows.
- `docs/container-image.md`, `Dockerfile`, `Dockerfile.macos`,
  `scripts/test/macos_entrypoint_test.sh`: pinned versions.
- `crates/execution/src/node/README.md`: file rows.
- `AGENTS.md` key modules: `node/` entry.

## Open Questions

None blocking. Decisions taken without a human (recorded per brief):
musl source, libc detection, preseed surface/layout/miss semantics and the
version bump — see `docs/toolu/brainstorms/2026-10-12-node-runtime-musl-node24.md`
(Jev-informed).

## Spec review (2026-10-12)

Adversarial pass against the checklist plus Jev requirement/evidence
alignment (issue 95-S1..S4 vs. ACs):

- Acceptance criteria: 🟡 should-fix (fixed): AC-2 flipped-flags case ignored the captured `warnonnode20 = "true"`; now asserts upstream's deprecation warning.
- Architecture: 🟡 should-fix (fixed): upstream prints the .NET enum spelling `Arm64`, not `ARM64`.
- Acceptance evidence: 🟡 should-fix (fixed): AC-1 relied on a hand-written truth table; now pins upstream's own `NodeUtilL0.cs` oracle rows verbatim (Jev weakest-AC choice `ac1` 0.84 → evidence-real 0.39 → 0.61).
- Acceptance evidence: 🟡 should-fix (fixed): AC-6 failure binaries were synthetic; now real `/bin/echo` and the real alpine binary on glibc.
- Acceptance evidence: 🔵 consider: live Linux arm64 / live musl host remain unverified (disclosed in Applicability; Jev S1 0.74). Non-blocking: the issue treats disclosed unverified lanes as unverified, not passing.

Jev: S2 0.92, S3 0.94, S4 0.93. No blockers remain.
