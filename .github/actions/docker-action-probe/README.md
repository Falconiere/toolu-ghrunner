# Docker action probe provenance

This directory is the committed real-container fixture for issue #75. Its base
image is the multi-platform `alpine:3.21` index resolved on 2026-09-25 and pinned
as `alpine@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507`.

The Runner replay starts from the sanitized GitHub.com V2 acquisition body in
`crates/toolu-runner/tests/fixtures/job_container_message.json`. That body and
its capture method are documented beside the fixture. The #75 replay deliberately
changes only these acquired fields:

- removes `jobContainer` except in the network-specific scenario;
- replaces checkout, Node, composite, verify, and upload steps with local Docker
  probe, registry Docker, and verification steps;
- points local action references at copies of this committed directory inside
  the acquired workspace;
- supplies explicit `with:` and step `env:` values needed for argv, precedence,
  failure, and cancellation cases.

`action.yml` covers pre/main/post, fixed manifest argv, default environment, file
commands, annotations, state, mounts, failure, and cancellation. The absent and
empty variants differ only in the `runs.args` field so fallback behavior is
observable without synthesizing a manifest in test code. The composite-registry
variant invokes pinned Alpine through nested `docker://`, writes a real workspace
marker, and maps the nested action's `GITHUB_OUTPUT` value through the composite
output. The false-stages variant uses literal false pre/post conditions, avoiding
an inputs context the official runner does not expose to those fields.
`DOCKER75|...` output is the live/reference log contract; `docker-stages.txt` is
the local replay contract.
The failure-post variant uses `post-if: failure()` and a real exit 17 from main.
The remote replay also requests exit 18 from pre and verifies its saved state
reaches post while main never runs. Summary content is checked through the real
file-command mount; backend summary upload is outside this probe's evidence.

The local Linux carrier can supply evidence for the production engine on Linux
ARM64. It defaults to `rust:1.94.1`; set
`TOOLU_DOCKER_ACTIONS_TEST_IMAGE=toolu-72-gate-tools:local` to reuse the local
gate-tools image.
`scripts/test/docker_actions_linux.sh remote` separately replays the pushed
action through the production GitHub downloader. It requires a 40-character
`TOOLU_DOCKER_ACTIONS_REMOTE_REF` and an authorized
`TOOLU_DOCKER_ACTIONS_REMOTE_TOKEN`; the carrier forwards the token without
printing or persisting it. Default `all` excludes this post-push lane.
The workflow's GitHub-hosted job is the reference lane. The opt-in self-hosted
job, x64 comparison, and GHES remain unverified until those external runners are
available; absence never counts as a pass.
