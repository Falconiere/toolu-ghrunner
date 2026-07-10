<div align="center">

# toolu-runner

**A self-hosted GitHub Actions runner, rewritten in Rust.**

One static binary. No .NET. No orchestrator service. No daemon you
didn't ask for.

[![ci](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/ci.yml/badge.svg)](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/ci.yml)
[![live](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/live.yml/badge.svg)](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/live.yml)
[![rust 1.94.1](https://img.shields.io/badge/rust-1.94.1-b7410e.svg)](rust-toolchain.toml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[Install](#install) · [Quick start](#quick-start) ·
[Watch a job](#watch-live-jobs-in-your-terminal) ·
[How it works](#how-it-works) ·
[Cache acceleration](#cache-acceleration) ·
[vs. `actions/runner`](#vs-actionsrunner) ·
[Docs](docs/architecture.md)

</div>

---

`toolu-runner` speaks the same JIT listener protocol as GitHub's own
runner — RSA key reconstruction → PS256 JWT → OAuth2 → broker session →
long-poll → execute → report. It runs your real workflows: shell steps,
Node.js actions, Docker actions, composite actions, reusable workflows,
matrices, `${{ }}` expressions, artifacts, cache, and OIDC.

The nightly [`live`](.github/workflows/live.yml) workflow above is not a
mock. It dispatches a real job to a real `toolu-runner` on a real repo,
every morning at 06:00 UTC.

> [!WARNING]
> **Pre-alpha (v0.1.0).** The live path is green nightly, but rough
> edges remain: `remove` doesn't yet call GitHub's unregister API, and
> there is no watchdog for network outages lasting more than 5 minutes
> mid-job. See [docs/known-bugs.md](docs/known-bugs.md) before you point
> this at anything you care about.

## Install

```sh
# macOS / Linux — installs to /usr/local/bin
curl -fsSL https://raw.githubusercontent.com/Falconiere/toolu-ghrunner/main/install.sh | sh

# ...or Homebrew
brew install falconiere/tap/toolu-runner
```

Add `--service` to also install and start the service unit (launchd on
macOS, systemd on Linux). Pass `--check` to print the plan and exit
without downloading anything.

Prebuilt for **macOS** (arm64, x86_64) and **Linux** (x86_64, arm64).

## Quick start

Log in once — GitHub's device flow opens your browser, no PAT to craft by
hand. The token is stored in your OS keyring (0600 file fallback where no
keyring exists):

```sh
# 1. Authenticate to GitHub
toolu-runner login

# 2. Register (repo URL or org URL both work) — no --token needed after login
toolu-runner register \
  --url https://github.com/owner/repo \
  --name my-runner \
  --labels self-hosted,linux,x64

# 3. Run the listener — blocks until SIGINT/SIGTERM
toolu-runner run

# 4. Watch jobs execute, in another terminal
toolu-runner watch
```

Prefer to manage the credential yourself? Pass `--token <pat>` (a PAT or App
installation token with `administration:write` on the repo/org) or set
`TOOLU_RUNNER_TOKEN`. Resolution order for `register` is
`--token` > `TOOLU_RUNNER_TOKEN` > stored login token. For GitHub Enterprise,
register an OAuth App on that instance and run
`toolu-runner login --hostname <ghes-host> --client-id <id>`.

`status` prints local state — including login — without touching the network.
`logout` deletes the stored token. `remove` unregisters. That's the whole CLI.

## Watch live jobs in your terminal

Every job writes a JSONL event journal to disk. `toolu-runner watch` is a
TUI over that journal — job history, a live step tree, streaming logs,
and a cancel key. No network, no server, no browser tab.

```
┌ toolu-runner watch ─────────────────────────────────────────────────────┐
│ runner: my-runner │ running · pid 48213 │                               │
└─────────────────────────────────────────────────────────────────────────┘
┌ jobs ──────────────────────────┐┌ build — running ───────────────────────┐
│ ● build          10:42:07      ││ ✓  1. Set up job                       │
│ ✓ test           09:18:22      ││ ✓  2. Checkout                         │
│ ✗ lint           08:55:01      ││ ●  3. cargo build --release            │
│ ⊘ deploy         08:31:44      ││ ○  4. Upload artifact                  │
│ ○ nightly        06:00:12      │└────────────────────────────────────────┘
│                                │┌ logs (follow) ─────────────────────────┐
│                                ││    Compiling protocol v0.1.0           │
│                                ││    Compiling toolu-runner v0.1.0       │
│                                ││     Finished `release` in 41.20s       │
└────────────────────────────────┘└────────────────────────────────────────┘
 q quit │ Tab pane │ ↑↓/jk move │ Enter open │ f follow │ PgUp/PgDn scroll │ c cancel
```

Logs are masked through the same `SecretMasker` that guards the runner's
own log file, so `secrets.*` values never land on disk in the clear.
`watch` also works with no runner running — it browses the last 50 job
journals under `~/.toolu-runner/_diag/jobs/`.

## How it works

```mermaid
sequenceDiagram
    participant R as toolu-runner
    participant GH as GitHub
    participant RS as Run / Results Service

    R->>GH: register (POST generate-jitconfig)
    GH-->>R: JIT config (RSA key, client id, urls)
    Note over R: RSA → PS256 JWT → OAuth2 token
    R->>GH: create broker session
    loop until SIGINT
        R->>GH: long-poll for a message
        GH-->>R: encrypted job (AES-256-CBC)
        R->>RS: acquire job
        Note over R: execute steps locally
        R->>RS: stream logs + step results
        R->>RS: renew lock (every 60s)
        R->>RS: complete job (conclusion)
    end
```

One process, one job at a time. The single-job guarantee is an `fs2`
file lock on `~/.toolu-runner/.lock` whose body carries the holder's
PID — a second `run` reads it, prints the PID, and exits `2`. Stale
locks (dead PID, mtime > 5 min) are reclaimed automatically.

`SIGINT`/`SIGTERM` are bridged to a `CancellationToken` that the poll
loop, the renewal task, and the in-flight job all observe. Nothing is
left orphaned.

### What runs

| | |
|---|---|
| **Steps** | `run:` shell, `uses:` Node.js actions (runtime auto-downloaded + cached), Docker actions, composite actions, plugins |
| **Workflows** | matrices, `needs:` job graphs, reusable workflows, `if:` conditions, `timeout-minutes`, `working-directory`, `defaults.run` |
| **Expressions** | the full `${{ }}` engine — lexer, parser, evaluator, `hashFiles`, `fromJSON`/`toJSON`, `contains`, `startsWith`, … |
| **Services** | artifacts, cache, and OIDC — forwarded to real GitHub by default, hosted locally in `offline` mode, or a local content-addressed cache accelerator in `accelerated` mode |
| **Safety** | secret masking across logs, stdout, and the journal; strict-mode clippy (no `unwrap`, no `panic`, no `unsafe`) |

### Service modes

`[services] mode` decides where artifacts, cache, and OIDC go.

- **`forwarder`** (default) — the runner reads the real GitHub service
  URLs and runtime token out of the job message and injects them into
  step env, so stock `upload-artifact@v4` / `cache@v4` / OIDC talk
  straight to GitHub. Drop-in compatible.
- **`offline`** — the runner hosts local stand-ins for those services.
  For airgapped hosts.
- **`accelerated`** — a local content-addressed cache intercepts
  GitHub Actions cache traffic (both the v2 Twirp `CacheService` and
  the legacy v1 REST protocol) and serves it from local NVMe,
  reverse-proxying everything else — artifacts included — to real
  GitHub. See [Cache acceleration](#cache-acceleration).

## Cache acceleration

In `accelerated` mode the runner hosts its own GitHub Actions cache. It
stores content-addressed, FastCDC-chunked blobs on local disk and
overrides **both** `ACTIONS_RESULTS_URL` (v2 Twirp) and
`ACTIONS_CACHE_URL` (legacy v1 REST) so that `actions/cache@v4`,
`docker buildx`'s `type=gha`, and older v1-only cache clients all hit
the local store instead of Azure. Everything that isn't cache — most
importantly `upload-artifact@v4` / `download-artifact@v4` — is
reverse-proxied verbatim to the real service, and `ACTIONS_RUNTIME_TOKEN`
stays the real GitHub token. Reads are shared across branches (chunks
are content-verified on every read); writes are branch-scoped, so a
`pull_request` job cannot poison a protected branch's cache. An
optional S3 cold tier (`[cache.l2]`) mirrors immutable chunks and
manifests off-box.

```toml
[services]
mode = "accelerated"     # "forwarder" (default) | "offline" | "accelerated"
bind = "0.0.0.0"         # must not be loopback — docker-container BuildKit reaches it here

[cache]
max_bytes          = 107374182400   # 100 GiB local (L1) budget
entry_ttl_days     = 7              # matches GitHub
protected_branches = ["main", "master"]
chunk_avg_bytes    = 65536          # FastCDC target chunk size

[cache.l2]                          # optional S3 cold tier
enabled  = false
bucket   = ""
endpoint = ""
region   = ""

[workspace]
gc_after_hours = 24      # prune _work/<job-id> older than this

[shadow]
enabled = false          # off by default; records would-hit/false-hit, never serves
```

### Docker: `buildx` needs `--driver-opt network=host`

`docker/build-push-action` and `docker buildx build --cache-to
type=gha` do **not** work against accelerated mode out of the box.
`type=gha` forces the `docker-container` driver, whose BuildKit runs in
its own network namespace and therefore cannot reach the runner's
loopback cache server. Create the builder with host networking so it
can:

```sh
docker buildx create --name toolu --driver-opt network=host --use
docker buildx build \
  --cache-to   type=gha,mode=max,url=http://127.0.0.1:<port>/ \
  --cache-from type=gha,url=http://127.0.0.1:<port>/ \
  .
```

The runner binds `0.0.0.0` (not loopback) precisely so a `network=host`
builder can reach it. This works on **native Linux docker**, where the
host namespace is the machine the runner runs on. On **Docker Desktop**
(macOS/Windows) `network=host` shares the Desktop VM's namespace, not the
host's, so `127.0.0.1` won't reach the runner — use the default bridge and
`--cache-to type=gha,url=http://host.docker.internal:<port>/` instead.

The `token=` attribute must be a **JWT** — BuildKit's cache client parses
it to read its scope claim before making any request, so an opaque string
is rejected. On a real GitHub-hosted job the injected `ACTIONS_RUNTIME_TOKEN`
already is one; only manual `buildx` invocations need to supply a JWT.

## vs. `actions/runner`

GitHub's runner is ~30K lines of C#. `toolu-runner` reimplements the JIT
listener subset in Rust, with a strict `sync protocol` → `async net`
boundary that keeps the crypto and wire-format code testable without a
clock, a socket, or tokio.

| Subsystem | `actions/runner` | `toolu-runner` |
|---|---|---|
| JIT config parse + RSA + JWT | C# | `protocol::auth` *(sync, no I/O)* |
| Token exchange / session | C# | `toolu-runner::net` |
| Message poll loop | C# | `listener::job_lifecycle` |
| Run service (acquire/renew/complete) | C# | `reporting::run_service` |
| Results service (Twirp) | C# | `reporting::results_service` |
| Expression engine (`${{ }}`) | C# | `execution::expressions` |
| Step handlers | C# | `execution::handlers` |
| Artifacts / cache / OIDC | C# | `execution::{artifacts,cache,oidc}` |
| Secret masking | C# | `execution::secret_masker` + tracing layer |
| Docker | C# | `docker::client` *(bollard)* |
| Node.js auto-download | C# | `node::runtime` |
| Live job TUI | — | **`toolu-runner watch`** |
| Plugin system | — | **`plugin::RunnerPlugin`** |

**Deliberately not ported:** OpenTelemetry, and any coupling to the
`yamless` orchestrator this code was extracted from. Both are rejected
at CI time.

**GHES** is supported over the V1 protocol (`connectionData` discovery,
timeline records); protocol version is auto-selected from the `--url`
host at `register` time.

## Configuration

<details>
<summary><code>~/.toolu-runner/config.toml</code> (mode 0600)</summary>

```toml
runner_url   = "https://github.com/owner/repo"
runner_name  = "my-runner"
runner_id    = 12345
auth_token   = "ghs_..."
labels       = ["self-hosted", "linux", "x64"]
runner_group = "Default"

[runtime]
jit_config       = "<base64 blob from GH>"   # written by `register`
work_dir         = "~/.toolu-runner/_work"
data_dir         = "~/.toolu-runner"
protocol_version = "v2"                      # "v1" for GHES

[services]
mode = "forwarder"   # "forwarder" (default) | "offline" | "accelerated"
```

The `accelerated` mode adds `[cache]`, `[cache.l2]`, `[workspace]`, and
`[shadow]` sections — see [Cache acceleration](#cache-acceleration).

Credentials live beside it in `credentials.json` (also 0600). Don't
hand-edit `jit_config` or `auth_token` — re-run `register --replace`.

</details>

<details>
<summary>Storage layout</summary>

```
~/.toolu-runner/
├── config.toml         # registration + runtime config (0600)
├── credentials.json    # OAuth token (0600)
├── .lock               # single-job lock (JSON: pid, started_at, config_path)
├── .pending_remove     # written by `remove` while a run is in flight
├── _work/              # per-job workspaces: <repo>/<job-id>/
├── _diag/
│   ├── runner.log      # JSON, secret-masked, daily-rotated
│   └── jobs/           # per-job JSONL journals (newest 50) — what `watch` reads
└── .runner_version
```

</details>

<details>
<summary>Environment variables</summary>

| Variable | Default | Description |
|---|---|---|
| `TOOLU_RUNNER_LOG` | `info` | tracing filter. Checked before `RUST_LOG`. |
| `RUST_LOG` | — | tracing filter (standard fallback). |
| `TOOLU_RUNNER_REPO` | `Falconiere/toolu-ghrunner` | `install.sh` only — release source. |
| `HOME` / `USERPROFILE` | — | resolves `~/.toolu-runner/`. |
| `HOSTNAME` / `COMPUTERNAME` | `unknown` | identifies the runner host at `register`. |
| `YAMLESS_*` | — | **Legacy.** Warned about, then ignored. No compatibility layer. |

`TOOLU_RUNNER_CONFIG` / `_WORK` / `_LABELS` are specced but **not yet
implemented** — use the CLI flags.

</details>

<details>
<summary>Running as a service</summary>

The release tarball ships service files under `scripts/`; `install.sh
--service` installs them.

**launchd (macOS)** — `scripts/io.toolu-runner.plist` →
`~/Library/LaunchAgents/`. Logs to
`/Users/Shared/toolu-runner/_diag/launchd-*.log`.

```sh
launchctl load   ~/Library/LaunchAgents/io.toolu-runner.plist
launchctl unload ~/Library/LaunchAgents/io.toolu-runner.plist
```

**systemd (Linux)** — `scripts/toolu-runner.service` →
`/etc/systemd/system/`. Runs as the `toolu-runner` user with
`NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`, `ProtectHome`,
`MemoryDenyWriteExecute`, and `Restart=always`.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now toolu-runner
sudo journalctl -u toolu-runner -f
```

</details>

## Troubleshooting

| Symptom | Fix |
|---|---|
| `config not found at ...` | Run `register` before `run`. |
| `registration already exists at ...` | Pass `--replace` to `register`. |
| `another run is in flight` | Another `run` holds `.lock`; its PID is in the error. Wait it out, or cancel with `c` in `watch` (sends SIGINT to the holder). |
| JIT endpoint probe fails at `register` | A firewall is blocking `pipelinesgh.azureedge.net` (github.com) or `pipelines.<host>` (GHES). |
| `warning: ignoring yamless env var ...` | A stale `YAMLESS_*` var is in your shell rc. Remove it. |

Job not showing up? Check the labels in `runs-on:` match the ones you
registered with, then `toolu-runner watch` to see what the runner
actually received.

## Development

Requires Rust 1.94.1 (pinned in `rust-toolchain.toml`).

```sh
cargo build --workspace
cargo test  --workspace          # 340 tests, no network required

./tools/check.sh all             # the full local gate
```

`tools/check.sh` is stricter than clippy: it rejects `.rs` files over
700 lines, `#[allow(..)]` / `#[expect(..)]` outside tests, `.unwrap()` /
`.expect()` in production code, and any `yamless` reference in source.
`lefthook install` wires the same checks to `pre-commit`.

The live suite talks to a real repo and is token-gated:

```sh
TOOLU_RUNNER_LIVE_TOKEN=<ghs_...> \
  cargo test --workspace --features e2e-live -- --ignored live
```

### Workspace

Three crates, one direction of dependency:

- **`shared`** — config, errors, events, job-message types, tracing init.
  Sync, I/O-free.
- **`protocol`** — JIT config, RSA/JWT, sessions, message decryption.
  Sync, I/O-free, **network-free** (no `reqwest`, no `tokio` — enforced
  by its `Cargo.toml`).
- **`toolu-runner`** — the lib + bin. Owns every socket, every `.await`.

[docs/architecture.md](docs/architecture.md) has the full design with
sequence diagrams for register / run / cancel / reconnect.

## Contributing

PRs welcome. Before you open one:

1. `./tools/check.sh all` passes.
2. `cargo test --workspace` passes.
3. Listener or reporting change? Add a test under `toolu-runner/tests/`.
4. User-facing change? Update `README.md`, `docs/architecture.md`, and
   `CHANGELOG.md` in the same commit.

New files stay under 700 lines; function bodies under 150.

## License

MIT — see [LICENSE](LICENSE).
