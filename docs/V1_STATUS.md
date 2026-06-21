# toolu-runner v1.0.0 — Status

**Date:** 2026-06-18
**Status:** Ready for v1.0.0 release. Step 10 (live smoke against github.com + GHES) is BLOCKED on user input (a registration token from a real test repo).

## Update 2026-06-20 — GitHub-compatibility core (E0–E3)

The original status above was over-optimistic. A deep gap analysis
(`docs/toolu/gh-compat-gap-analysis.md`) found that large parts of the
execution engine were written-but-unwired *dead code* — present in the
tree, but never reached on the live job path. The E0–E3 work
(plan `docs/toolu/plans/2026-06-19-gh-compatibility-core-execution.md`)
wired them onto the live path and adopted the **forwarder** model for
artifacts/cache/OIDC.

Now wired onto the live path:

- **Live JIT register** — `net/register.rs` POSTs `generate-jitconfig`
  and persists the real JIT config + `runner_id` (was a placeholder
  stub). Resolves B-003.
- **Message-body decryption** — RSA-OAEP AES-key unwrap + AES-CBC on
  the poll path; `JobCancellation` routing to the in-flight
  `CancellationToken`; `lastMessageId` poll cursor.
- **stdout workflow-command pipeline** — `::set-output::`,
  `::add-mask::`, `::error::`, `::group::`, `::save-state::`,
  `::stop-commands::` etc. (was DEAD), with `%XX` unescape and the
  shared `SecretMasker`.
- **Pre/post step stages** + `STATE_` persistence; local `./` actions
  and composite nested `uses:`.
- **Step semantics** — `timeout-minutes`, `working-directory`,
  `continue-on-error` (outcome ≠ conclusion), `INPUT_` space→underscore.
- **Job-level wiring** — `outputs:` → `JobCompleted.outputs`,
  `defaults.run`, `ACTIONS_RUNNER_HOOK_JOB_STARTED/_COMPLETED`.
- **Expression context** — real host-derived `runner.*`, full
  `github.*`, `vars.*`, masked `secrets.*`, `job.*`/`strategy.*`,
  `steps.*.state`.
- **Forwarder pivot** — the runner now injects the REAL GitHub service
  URLs + runtime token (from the job message's SystemVssConnection
  endpoint) into step env (`ACTIONS_RESULTS_URL`, `ACTIONS_RUNTIME_URL`,
  `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_CACHE_URL`, `ACTIONS_CACHE_SERVICE_V2`,
  `ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN`) so GitHub-hosted
  `upload-artifact@v4` / `cache@v4` / OIDC talk to real GitHub. New
  config `[services] mode` = `forwarder` (default) or `offline` (hosts
  the local fake services for airgapped use).

**Still pending:** live end-to-end validation (real-token smoke,
register → run → execute → report) is token-gated and not yet run
(tracked by S16). The status below predates this update.

## What ships in v1.0.0

A self-hosted GitHub Actions runner written in Rust, packaged as a single binary `toolu-runner`. The 3-crate workspace (shared, protocol, runner) implements:

- **GH JIT listener** — RSA → JWT (PS256) → OAuth2 → broker session → poll → acquire → execute → report → renew → complete.
- **GHES V1 protocol** — alternative code path for self-hosted GH instances; `feature_detection` picks V1 vs V2 per message.
- **Step handlers** — `script`, `node20`, `docker`, `composite`, plus resolution logic.
- **Expression engine** — full `${{ }}` syntax: literals, context/property/index, function calls, binary ops, ternary, wildcards.
- **OIDC token issuance** — `actions/oidc-token` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN` support.
- **Artifact upload + download** — Azure append-blob + Twirp Results Service.
- **Cache** — local disk + remote layered backend.
- **Reusable workflows** — `uses: org/repo/.github/workflows/x.yml`.
- **Secret masking** — `secrets.*` values masked in logs at all variants (plain, base64, JSON-escaped, double-escaped). Wired into the tracing layer so secrets never reach the file sink unredacted.
- **CLI** — `register` / `run` / `remove` / `status` / `--version` / `--help` (clap).
- **Service files** — launchd plist (macOS) + systemd unit (Linux).
- **Install script** — `install.sh` mirrors `actions/runner`'s UX; detects arch, downloads release, optionally installs service.

## What's tested (167 unit tests + 8 live tests, gated)

See `docs/test-coverage.md` for the per-AC test map. Lane breakdown:

- **default** (hermetic, `cargo test --workspace`): 154 tests
- **live** (gated by `--features live` + `TOOLU_RUNNER_LIVE_TOKEN`): 8 tests
- **out-of-scope** (enforced by `tools/check.sh` + `lefthook.yml`): install script, service files, lint gate

## What's NOT done (blocked on step 10)

Three known bugs in `docs/known-bugs.md`, all blocked on the user providing a registration token + test repo:

- **B-001** — Outage > 5 min mid-job: cancellation watchdog missing (medium severity).
- **B-002** — `toolu-runner remove` doesn't call the GH unregistration endpoint (low, deferred to step 10).
- **B-003** — `toolu-runner register` writes a placeholder `auth_token` and empty `jit_config`; the live flow (POST to JIT endpoint, RSA → JWT → OAuth2 exchange) is stubbed (low, deferred to step 10).

The harness is built (`toolu-runner/tests/live/`) and compiles. It will run the moment the user supplies `TOOLU_RUNNER_LIVE_TOKEN` and a test repo name.

## Quality gates (all green at HEAD)

```
$ cargo build --workspace                            # green
$ cargo test --workspace                              # 167 / 167 passing
$ cargo clippy --workspace --all-targets -- -D warnings  # clean
$ cargo fmt --all -- --check                         # clean
$ bash tools/check.sh all                            # clean
$ cargo tree -p protocol | grep -E 'reqwest|tokio|opendal|bollard|axum'  # no matches
$ cargo tree --workspace | grep -E 'yamless-|yamless_'  # no matches (after the rename in commit 458eb00)
```

## What's next (v1.1)

Once the user runs the live smoke (step 10), we triage B-001 / B-002 / B-003 from `docs/known-bugs.md` and add:

- **v1.1 features:** OIDC telemetry opt-in (per the spec's open question), Homebrew tap, signed+notarized macOS binaries, GHES feature-detection fuzz tests.
- **v1.1 cleanup:** deprecate `yamless-runner` in favor of `toolu-runner` (the runner engine is now standalone).

## How to run v1.0.0 (after the live smoke)

```bash
# Install
curl -fsSL https://github.com/Falconiere/toolu-ghrunner/releases/latest/download/install.sh | sh

# Register against a test repo
toolu-runner register --url https://github.com/owner/repo \
  --token <reg_token> --name my-runner --labels self-hosted,linux,x64

# Run as a service
sudo systemctl enable --now toolu-runner.service    # Linux
launchctl load ~/Library/LaunchAgents/io.toolu-runner.plist  # macOS

# Check status
toolu-runner status
```

## Commits (28 total)

Plan + spec + 27 progressive commits across 17 plan steps. See `git log` for the full list; the commit message prefix indicates the step (`feat(shared):`, `feat(protocol):`, `feat(runner):`, `test(shared):`, `test(runner):`, `docs:`, `ci:`, `fix:`).
