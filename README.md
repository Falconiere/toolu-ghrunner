# toolu-runner

Standalone self-hosted GitHub Actions runner written in Rust. Runs real jobs
against github.com and GHES, with no orchestrator service in the loop.

> **Status: pre-alpha.** v0.1.0 is the first release. The full live smoke
> (register → run → execute → report against a real repo) is gated on
> step 10 of the build plan, which requires a registration token from a
> test repo. Until then, `register` validates the URL and writes a
> placeholder config; `run` requires a live JIT config to actually poll.

## Quick start

```bash
# Install (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/Falconiere/toolu-ghrunner/main/install.sh | sh

# Register against a GitHub repo
toolu-runner register --url https://github.com/owner/repo \
  --token <reg_token> --name my-runner --labels self-hosted,linux,x64

# Run the listener (blocks until SIGINT/SIGTERM)
toolu-runner run

# Check local state (no network)
toolu-runner status

# Unregister
toolu-runner remove
```

The `--url` accepts a repo URL (`https://github.com/owner/repo`) or an
org URL (`https://github.com/org`). The registration token comes from
the repo or org's **Settings → Actions → Runners → New self-hosted
runner** page.

See [docs/architecture.md](docs/architecture.md) for the full design
and [docs/known-bugs.md](docs/known-bugs.md) for the live-smoke
caveats.

## How it works

`toolu-runner` is a single binary that:

1. Registers with GitHub (github.com or GHES) using a short-lived
   registration token from the repo/org's Runners page. The JIT
   endpoint is auto-derived from the `--url` host
   (`pipelinesgh.azureedge.net` for github.com, `pipelines.<host>` for
   GHES).
2. Polls the Actions Run Service for job assignments over the JIT
   protocol. The auth chain is RSA key reconstruction → JWT (PS256) →
   OAuth2 token exchange → broker session → long-poll message
   acquisition.
3. Executes the job locally: shell scripts, Node.js actions
   (auto-downloaded), Docker actions, composite actions, reusable
   workflows, artifacts, cache, and OIDC tokens. Step results are
   reported back through the Results Service (Twirp).
4. Renews the job lock every 60s and streams step logs to the
   Results Service as the job runs.
5. Completes the job with the final conclusion, then loops back to
   the poll.

The listener is one process. The single-job guarantee is enforced by
a file lock on `~/.toolu-runner/.lock` (see [Storage layout](#storage-layout)).

## Comparison to upstream `actions/runner`

`actions/runner` is a ~30K-line C# binary that the GitHub team ships.
`toolu-runner` reimplements the JIT listener subset in Rust, with no
orchestrator service in the loop:

| Subsystem                       | actions/runner | toolu-runner |
|---------------------------------|----------------|--------------|
| JIT config parse + RSA + JWT    | C#             | `protocol::auth` (sync, no I/O) |
| Token exchange / session        | C#             | `toolu-runner::net` (async reqwest) |
| Message poll loop               | C#             | `listener::job_lifecycle` |
| Run service (acquire/renew/complete) | C#        | `reporting::run_service` + `net::run_service` |
| Results service (Twirp)         | C#             | `reporting::results_service` + `net::results_service` |
| Expression engine (`${{ }}`)    | C#             | `execution::expressions` |
| Step handlers                   | C#             | `execution::handlers` (script, node, composite, docker) |
| Artifacts / cache / OIDC        | C#             | `execution::artifacts` / `cache` / `oidc` |
| Secret masking                  | C#             | `execution::secret_masker` + tracing layer |
| Docker integration              | C#             | `docker::client` (bollard) |
| Node.js auto-download           | C#             | `node::runtime` |
| Plugin system                   | none           | `plugin::RunnerPlugin` (new) |

**Not ported (out of scope for v1):** the yamless-orchestrator WebSocket
client, yamless-specific step handlers (`yamless_deploy`,
`yamless_notify`, `yamless_test_report`), and the OpenTelemetry layer.
See [docs/known-bugs.md](docs/known-bugs.md) for the live-smoke status.

## Supported platforms

- **macOS** — arm64 (Apple Silicon), x86_64 (Intel)
- **Linux** — x86_64, arm64

The runner is built and tested against the `stable` Rust toolchain
(pinned in `rust-toolchain.toml`). It depends on:

- `bollard` (Docker client) — requires a running Docker daemon on the
  host for Docker actions.
- `tokio` (async runtime), `reqwest` (HTTP), `axum` (artifact / cache
  / OIDC micro-services).
- `tokio-tungstenite` (WebSocket for live log streaming).
- System `cgroup v2` is *not* required (v1 runs in the user's session;
  isolation is a v1.1 feature).

## Development

Requires Rust 1.94.1 (pinned in `rust-toolchain.toml`).

```sh
# Build everything
cargo build --workspace

# Run all unit tests
cargo test --workspace

# Run the live smoke (requires a registration token from a test repo)
TOOLU_RUNNER_LIVE_TOKEN=<ghs_...> \
  cargo test --workspace --features e2e-live -- --ignored live

# Lint (denies all warnings)
cargo clippy --workspace --all-targets -- -D warnings

# Format check
cargo fmt --all -- --check

# Local quality gate (fmt + clippy + file-size + no-allow + no-unwrap + no-yamless)
./tools/check.sh all
```

`tools/check.sh` mirrors the yamless backend's check script: rejects
`.rs` files over 700 lines, rejects `#[allow(..)]` / `#[expect(..)]`
outside tests, rejects `.unwrap()` / `.expect()` in production code,
and rejects any `yamless` / `YAMLESS_` reference in source.

`lefthook` runs `fmt --check`, `clippy`, and the no-yamless-coupling
check as a `pre-commit` hook:

```sh
lefthook install   # one-time
lefthook run pre-commit
```

## Service install

The release tarball ships service files at `scripts/`. `install.sh`
installs them with `--service`.

**launchd (macOS):** `scripts/io.toolu-runner.plist` lands in
`~/Library/LaunchAgents/`. Override the `--config` path in the plist
if you store `config.toml` somewhere other than the default
(`/Users/Shared/toolu-runner/config.toml`).

```sh
# After install:
launchctl load ~/Library/LaunchAgents/io.toolu-runner.plist
launchctl unload ~/Library/LaunchAgents/io.toolu-runner.plist   # to stop
```

The plist sets `TOOLU_RUNNER_LOG=info` and pipes `StandardOutPath` /
`StandardErrorPath` to `/Users/Shared/toolu-runner/_diag/launchd-*.log`.

**systemd (Linux):** `scripts/toolu-runner.service` lands in
`/etc/systemd/system/`. It runs as the `toolu-runner` user/group with
hardened sandboxing (`NoNewPrivileges`, `ProtectSystem=strict`,
`PrivateTmp`, `ProtectHome`, `MemoryDenyWriteExecute`, etc.) and
`Restart=always` for crash recovery. Logs go to the journal under
`SyslogIdentifier=toolu-runner`.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now toolu-runner
sudo journalctl -u toolu-runner -f   # follow logs
```

Service test scripts live at `scripts/test/plist_test.sh` (macOS)
and `scripts/test/systemd_test.sh` (Linux). They are smoke checks
that the unit file parses, not end-to-end service bring-up tests.

## Environment variables

| Variable                   | Default                  | Used by              | Description |
|----------------------------|--------------------------|----------------------|-------------|
| `TOOLU_RUNNER_LOG`         | `info` (EnvFilter)       | all subcommands      | tracing log filter. Used first; falls back to `RUST_LOG` then `info`. |
| `RUST_LOG`                 | (passes through)         | all subcommands      | tracing log filter (standard). |
| `TOOLU_RUNNER_REPO`        | `Falconiere/toolu-ghrunner` | `install.sh` only | GitHub owner/repo to download the release from. |
| `HOME`                     | —                        | `register` / `run`   | Resolves `~/.toolu-runner/` for the default data dir. |
| `USERPROFILE`              | —                        | `register` / `run`   | Windows fallback for `HOME`. |
| `HOSTNAME` / `COMPUTERNAME`| `unknown`                | `register`           | Used by the session registration to identify the runner host. |
| `YAMLESS_*` (any)          | —                        | all subcommands      | **Legacy.** The runner prints `WARN: ignoring legacy env var {key} — toolu-runner has no compatibility layer for the old prefix; use TOOLU_RUNNER_* instead` for each and ignores. |

The spec also lists `TOOLU_RUNNER_CONFIG`, `TOOLU_RUNNER_WORK`, and
`TOOLU_RUNNER_LABELS` as future env-var overrides for the
`--config` / `--work` / `--labels` flags. **These are not yet
implemented** — the CLI reads the flags directly. Use the flags
for v0.1.0.

## Configuration

`toolu-runner register` writes a `config.toml` (mode 0600) and a
`credentials.json` (mode 0600) under `~/.toolu-runner/`. The schema
mirrors what the code parses in `toolu-runner/src/config.rs`:

```toml
# ~/.toolu-runner/config.toml
runner_url   = "https://github.com/owner/repo"
runner_name  = "my-runner"
runner_id    = 12345
auth_token   = "ghs_..."
labels       = ["self-hosted", "linux", "x64"]
runner_group = "Default"

[runtime]
jit_config       = "<base64 blob from GH>"   # populated by `register`
work_dir         = "~/.toolu-runner/_work"
data_dir         = "~/.toolu-runner"
protocol_version = "v2"                       # "v1" for GHES
```

```json
// ~/.toolu-runner/credentials.json
{
  "access_token": "ghs_...",
  "issued_at": "2026-06-18T10:00:00Z",
  "expires_at": null
}
```

Do not edit `jit_config` or `auth_token` by hand — re-run `register`
with `--replace` to regenerate them.

### Storage layout

```
~/.toolu-runner/
├── config.toml                 # registration + runtime config (0600)
├── credentials.json            # long-lived OAuth token (0600)
├── .lock                       # single-job file lock (0600, JSON body)
├── .pending_remove             # marker written by `remove` while a run is in flight
├── _work/                      # per-job workspaces
│   └── <repo>/
│       └── <job-id>/
├── _diag/                      # log files, diagnostic dumps
│   ├── runner.log              # JSON, secret-masked, daily-rotated
│   └── runner.log.YYYY-MM-DD   # rotated archives
└── .runner_version             # installed toolu-runner version
```

The `.lock` body is JSON: `{"pid": 12345, "started_at":
"2026-06-18T10:00:00Z", "config_path": "/Users/.../config.toml"}`. A
second `toolu-runner run` that finds the lock held reads the body,
prints the PID, and exits 2. A stale lock (holder PID dead and mtime
older than 5 min) is removed and re-acquired by the next `run`.

## Known bugs

See [docs/known-bugs.md](docs/known-bugs.md) for the current list. The
short version: the live `register` POST and live `remove` unregister
call are stubbed pending step 10 (live smoke against a test repo).
The 5-min cancellation watchdog on prolonged mid-job network outages
is also tracked as a known gap.

## Troubleshooting

- **"config not found at ..."** — `register` first, then `run`.
- **"registration already exists at ..."** — pass `--replace` to
  `register` to overwrite.
- **"another run is in flight"** — another `toolu-runner run` is
  holding `.lock`. Re-run `remove` with `--force` to cancel it, or
  wait for the job to finish. The PID and start time are in the
  error message.
- **"warning: ignoring yamless env var YAMLESS_*"** — you have a
  yamless shell profile still set. `toolu-runner` does not read any
  `YAMLESS_*` variables; remove them from your shell rc.
- **JIT endpoint probe fails at `register`** — the runner does a HEAD
  to the JIT endpoint derived from `--url`'s host. Network
  restrictions or a firewall that blocks `pipelinesgh.azureedge.net`
  (or `pipelines.<host>` for GHES) will surface here.
- **"bash -n install.sh" fails** — re-download the install script;
  older yamless runner install scripts in the wild had a different
  flag set.

## Contributing

This is a docs-and-tests-driven project. Before opening a PR:

1. Run `./tools/check.sh all` and ensure it passes.
2. Run `cargo test --workspace` and ensure all tests pass (currently 196).
3. If your change touches the listener or reporting, add a unit test
   in `toolu-runner/tests/` that exercises the new code path.
4. If your change is public-facing, update `README.md` /
   `docs/architecture.md` / `CHANGELOG.md` in the same commit.

The repo is governed by a strict clippy config (see `Cargo.toml`):
no `unwrap()` / `expect()` outside tests, no `#[allow(..)]` /
`#[expect(..)]`, no yamless coupling. New files must stay under
700 lines (enforced by `tools/check.sh`; function-body cap is 150 lines via clippy's `too_many_lines`).

## License

MIT — see [LICENSE](LICENSE).
