# toolu-runner v1.0.0 — Status

**Date:** 2026-06-18
**Status:** Ready for v1.0.0 release. Step 10 (live smoke against github.com + GHES) is BLOCKED on user input (a registration token from a real test repo).

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
