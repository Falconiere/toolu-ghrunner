# Captured job-container message

`job_container_message.json` is the sanitized GitHub.com V2 acquisition body
from [run 36054305027](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36054305027),
acquired on 2026-09-24. Workflow/source revision:
`44aedf38e95137ad6128a14db84475e8048cd791`. The run reached shell and Node
execution, then failed resolving the captured local composite reference.

The capture was written from `wire::net::run_service::acquire_job` after JSON
decoding, using temporary instrumentation in an isolated Linux source snapshot.
The delivered runner has no raw-capture option. Original body SHA-256:
`029c4f46c33b20950e64b72db053e6957a8ea2f15d78c4d49338da8693d327b1`.

Sanitization replaces secret variable values, endpoint authorization values,
all 24 mask variants, and JWT strings with explicit redaction markers. Service
URLs become `actions.example.invalid`. Public repository/workflow metadata,
actual UUIDs and `contextName`, reference fields, token types, map order and
source coordinates remain intact. No credential scan exemption is requested.
The original private body and JIT credentials are not fixtures.

Image:
`ubuntu@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3`.
Node probe revision: `dec29e817264994f7e8e1f49824647f739c7c063`.
The reference lane uses unmodified actions/runner 2.337.0
(`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`), linux-arm64 archive SHA-256
`9b1dc70626422526e3c94767cf024896beb15da5342a3f4819bf2feac13e0393`.
Both dedicated runner processes use one local Linux Docker daemon with isolated
homes and labels. This is not two independent physical machines or GHES evidence.

Official reference run
[36054931184](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36054931184)
succeeded at the same workflow SHA. All six stage/command markers were present;
`container-73.txt` contained exactly `container-73-artifact\n`; the artifact-recorded
container and network were absent from Docker after completion. The original toolu
run failed, so this does not establish toolu/reference parity.

The captured replay passed after fixing self/path-only local action references
and projecting step outputs under `contextName` while retaining internal UUIDs.
It removes only checkout and upload-artifact and stages the pinned local composite
manifest; all four remaining captured steps, including final named-output
verification, execute unchanged. Three parse/Linux replay checks passed with
real Docker (14.112 s); this is distinct from live artifact backend parity.

After those fixes, the paired live harness passed in 67.337 s:
[toolu 36057599244](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36057599244)
and [official 36057648598](https://github.com/Falconiere/toolu-ghrunner/actions/runs/36057648598).
Both used workflow SHA `44aedf38e95137ad6128a14db84475e8048cd791`; the toolu
binary additionally contained the captured-reference/context-name fixes delivered
with this fixture. All six markers, exact artifact bytes and owned-resource
cleanup passed. This evidence covers the paired workflow, not #74/#75 or GHES.
