# Runner versions and updates

Toolu advertises **2.338.2** as its GitHub protocol compatibility target in
session creation, idle and busy broker polls, and job acknowledgement. The source
is `protocol::runner_version::COMPATIBILITY_VERSION`, pinned to the
[official v2.338.2 release](https://github.com/actions/runner/tree/v2.338.2)
(`d1eb12d4b115c301201de27d667284e1a80f2ab5`). The earlier 2.337.0 baseline
([release](https://github.com/actions/runner/tree/v2.337.0),
inspection snapshot `cab9d1c`) is an ancestor; `docs/compat-reference-103.json`
stays attributed to 2.337.0, the version its paired run used.
This replaces the invented `3.0.0` identity and the inconsistent acknowledgement
version. It is a compatibility target, not a claim that toolu implements every
feature of that runner or that GitHub guarantees queue eligibility.

Toolu's product/release version remains independent: `toolu-runner --version`
prints it, and listener startup logs both `product_version` and
`compatibility_version`. `toolu-runner doctor` shows both on its `version`
row, and a job's "Set up job" log lists them as `Toolu runner version` and
`Runner compatibility version`. Use both when reporting an incident. Updating
toolu's Cargo version does not automatically raise the compatibility target.

## Chosen update policy

Updates are **operator-managed**. Polling sends `disableUpdate=true`.
`RunnerRefresh` and `AgentRefresh` produce an operator warning, advance the
broker cursor and continue polling. They never download or execute an update,
change a binary, or interrupt an active job. A duplicate or older message ID is
suppressed by the existing cursor check. Bodies are opaque and are not logged.
A later valid job is handled normally. `RunnerRefreshConfig` remains an
unsupported configuration request; `ForceTokenRefresh` is separate OAuth token
renewal and still works through its own handler.

This policy applies to `run`, `run --once`, `boot`, and daemon-managed listeners.
Install a validated release from **Falconiere/toolu-ghrunner** (or rebuild your
pinned toolu deployment image), using the installation method that owns the
instance. GitHub's official-runner update packages cannot update a toolu binary.
No broker message or version field triggers a toolu self-update.

## Maintenance ownership and deadline

The **toolu-runner release maintainer** owns monitoring `actions/runner` releases,
reviewing protocol/security changes, updating the compatibility baseline after
validation, and publishing the toolu release with its evidence. The **operator
responsible for each runner fleet** owns deploying that release, rebuilding
images and restarting all instances. Assign these responsibilities to named
people in your deployment runbook; a published release alone does not update a
running process or a cached image.

GitHub's [self-hosted runner update policy](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#runner-software-updates-on-self-hosted-runners)
requires runners with automatic updates disabled to update within **30 days of
any new runner release**, including patches. A required critical security update
can stop job assignment until installed, without that grace period. Missing the
deadline can leave jobs queued even while a runner appears online. Monitor
release notifications and queue age; treat a required security update as urgent.

The service's treatment of alternative implementations is not a published
compatibility guarantee. A higher advertised number is not a workaround, and a
local clock simulation cannot prove GitHub's eligibility behavior. Consult the
[coverage map](test-coverage.md#runner-version-and-updates-issue-78) for observed
service acceptance and unverified lanes. GHES eligibility must be revalidated
against the actual supported server version; GitHub.com observations do not
certify GHES.

## Update and revalidation procedure

1. Record the new official tag, source commit, release date/security deadline,
   and changes affecting toolu's supported behavior. Implement necessary changes;
   do not merely replace the version string to suppress gating.
2. Update the protocol constant deliberately. Update the independently pinned
   wire regression expectations and document the new baseline/rationale.
3. Run `./tools/check.sh all` and the relevant captured-message/real-tool tests.
   Provision isolated toolu and pinned official runners on equivalent hosts;
   run the same committed workflow/action revisions on both.
4. Verify real registration, session, poll, acquire, acknowledgement and completion
   plus the GitHub job conclusion. Record source/binary versions, host OS/arch,
   workflow SHA and run links. Repeat on each supported backend/server version.
   Missing credentials or a skipped lane is unverified, never a pass.
5. Publish a toolu release with the new evidence and scope. Operators drain jobs,
   replace the binary or image, restart the service/container, mint fresh JIT
   registrations, and confirm the startup identities and a completed canary job.

If jobs remain queued or version rejection appears, check labels/capacity and
service errors alongside the current GitHub update requirement. Stop scheduling
the affected fleet, deploy a validated toolu release and run the canary again.
If no validated toolu release is available, route work to a supported, updated
official runner while the release maintainer resolves compatibility. Repeated
reminting or raising the compatibility number is not evidence of recovery.

## Delta table: v2.337.0 to v2.338.2 (#241)

Source: `gh api repos/actions/runner/compare/v2.337.0...v2.338.2` (41 commits,
44 files). Dependency, Docker/Buildx, CodeQL and CI bumps change no runner
behavior and are omitted. Live pairing at this pin is #251.

| Upstream change | Toolu status | Evidence |
| --- | --- | --- |
| [#4669](https://github.com/actions/runner/pull/4669) `$/` self-repository refs in action manifests (flag `actions_self_repository`) | Fixed for nested composite `uses:`; a top-level `$/` step is listed in [known bugs](known-bugs.md) | `cargo test -p execution --test integration -- self_repository_test:: composite_self_repository_test::` |
| [#4743](https://github.com/actions/runner/pull/4743) owned session id on broker session delete | Fixed: create sends `brokerSession: true`; delete is `DELETE /session?sessionId=<id>` | `cargo test -p wire --test integration -- runner_version::` |
| [#4753](https://github.com/actions/runner/pull/4753) broker WebSocket probes | Not applicable: temporary telemetry, only when the job sets `system.runner.brokerwebsocketprobeurl`; never changes a job result | Source review |
| [#4752](https://github.com/actions/runner/pull/4752) node24 for internal operations | Not applicable: the official listener's own bundled Node; toolu runs no internal Node | Source review |
| [#4685](https://github.com/actions/runner/pull/4685) Node 20 removal date | Already `September 23rd, 2026` (`node/migration.rs`) | `cargo test -p execution --test integration -- node_migration_test::` |
| `ACTIONS_ORCHESTRATION_ID` (`ScriptHandler`) | Fixed: `run:` steps get `system.orchestrationId` when `actions_set_orchestration_id_env_for_actions` is true | `cargo test -p toolu-runner --test integration -- gh_compat_forwarder::orchestration_id` |
| 2.338.0 to 2.338.2 release commits | Version files only | Source review |
