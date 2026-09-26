# Runner versions and updates

Toolu advertises **2.337.0** as its GitHub protocol compatibility target in
session creation, idle and busy broker polls, and job acknowledgement. The source
is `protocol::runner_version::COMPATIBILITY_VERSION`, pinned to the
[official v2.337.0 release](https://github.com/actions/runner/tree/397b032cbf865e9c3ddfab89d533ec19325e1273).
The epic's later [source-inspection snapshot](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/runnerversion)
still reports the same version; it is distinct from the release commit.
This replaces the invented `3.0.0` identity and the inconsistent acknowledgement
version. It is a compatibility target, not a claim that toolu implements every
feature of that runner or that GitHub guarantees queue eligibility.

Toolu's product/release version remains independent: `toolu-runner --version`
prints it, and listener startup logs both `product_version` and
`compatibility_version`. Use both when reporting an incident. Updating toolu's
Cargo version does not automatically raise the compatibility target.

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
