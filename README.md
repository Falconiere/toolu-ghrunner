<div align="center">

# toolu-runner

**A self-hosted GitHub Actions runner, rewritten in Rust.**

One native binary. No .NET. No orchestrator service. No daemon you
didn't ask for.

[![ci](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/ci.yml/badge.svg)](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/ci.yml)
[![live](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/live.yml/badge.svg)](https://github.com/Falconiere/toolu-ghrunner/actions/workflows/live.yml)
[![rust 1.94.1](https://img.shields.io/badge/rust-1.94.1-b7410e.svg)](rust-toolchain.toml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[Install](#install) · [Quick start](#quick-start) ·
[How it works](#how-it-works) ·
[Cache acceleration](#cache-acceleration) ·
[vs. `actions/runner`](#vs-actionsrunner) ·
[Container image](docs/container-image.md) ·
[macOS image](docs/macos-image.md) ·
[VPS host runbook](docs/vps-host-runbook.md) ·
[Architecture](docs/architecture.md) ·
[Compatibility](docs/compatibility-matrix.md) ·
[Known limitations](docs/known-bugs.md)

</div>

---

`toolu-runner` speaks the same JIT listener protocol as GitHub's own
runner — RSA key reconstruction → PS256 JWT → OAuth2 → broker session →
long-poll → execute → report. It runs your real workflows: shell steps,
Node.js actions, composite actions, reusable workflows,
matrices, `${{ }}` expressions, artifacts, cache, and OIDC.

Composite action steps evaluate expressions against their own inputs and inner
step results. A failing inner step still allows eligible `failure()` and
`always()` cleanup, while nested action state and post actions stay scoped to
their invocation. Composite `working-directory` values are expression-evaluated
for each run step.
Relative paths resolve from the workspace; absolute paths are preserved. Missing
or empty values use the workspace, independently of caller `defaults.run`.
A missing directory fails that inner step and still permits eligible cleanup.

The manually dispatched [`live`](.github/workflows/live.yml) workflow above is
not a mock. It dispatches a real job to a real `toolu-runner` on a real repo.
It has no schedule (#120) and fails as `UNVERIFIED` when
`TOOLU_RUNNER_LIVE_TOKEN` is not set.

> [!WARNING]
> **Pre-1.0.** The live path is only verified when `live` is dispatched with credentials, and rough edges remain:
> Container actions require Linux and a local Docker daemon. Acceptance
> evidence and remaining platform gaps are tracked in
> [docs/test-coverage.md](docs/test-coverage.md). See [docs/known-bugs.md](docs/known-bugs.md) before
> you point this at anything you care about.

## Install

```sh
# macOS / Linux — installs, then hands the terminal to `toolu-runner setup`
curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash

# ...install only
curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash -s -- --no-setup

# ...unattended (cloud-init): setup flags after `bash -s --`, the token in the env
curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | TOOLU_RUNNER_TOKEN="$PAT" \
  bash -s -- --source github --repo acme/web --auth token --yes

# ...unattended toolu.sh enrollment (the console one-liner); `bash -s --` hands `--toolu` to the installer
curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash -s -- --toolu <enrollment-token>

# ...force /usr/local/bin on a host where it needs root
curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | sudo bash

# ...or pick the directory yourself
curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash -s -- --install-dir "$HOME/bin"

# ...or Homebrew
brew install falconiere/tap/toolu-runner
```

What the installer does:

- **Downloads** `releases/latest/download/<asset>` (or `--version vX.Y.Z`) —
  no GitHub API call, so no `jq` — and **verifies** it against that release's
  `SHA256SUMS` with `sha256sum` (or `shasum -a 256` on macOS). A mismatch, or a
  `SHA256SUMS` without the asset, stops before anything is installed. The
  version it reports comes from the installed binary's `--version`.
- **Installs** to `/usr/local/bin` when it is writable, otherwise to
  `~/.local/bin` (no sudo needed). `--install-dir` is used as given.
- **Sets up PATH for new shells** when the install dir is not on it already:
  it writes `~/.toolu-runner/env` and `env.fish` and sources them from
  `~/.profile`, `~/.bashrc` (plus `~/.bash_profile` / `~/.bash_login` when
  present), `${ZDOTDIR:-~}/.zshenv` when zsh is installed or that file
  exists, and
  `${XDG_CONFIG_HOME:-~/.config}/fish/conf.d/toolu-runner.fish` — one marker block per file, so a
  re-run changes nothing. `--home <dir>` also exports `TOOLU_RUNNER_HOME` there.
  These files never hold a secret. `--no-modify-path` writes none of them and
  prints the line to add; `--uninstall` removes the blocks and files again,
  leaving the rest of each profile byte-identical (a profile left empty, such
  as one the installer created, is deleted; the binary and runner state stay). A
  file that cannot be written is a warning with the line to add, never a
  failed install.
- **Hands off to `toolu-runner setup`**: on a terminal it runs setup
  interactively (reading `/dev/tty`, since under `curl | bash` stdin is the
  script); every option the installer does not own is passed to setup in
  order. With no terminal it runs setup only when setup options were given,
  and otherwise prints one `next:` line. A release without a `setup` command
  points at `register` instead, and setup options given to it exit 2.

Output follows the setup screens: `✓` rows and colour on a terminal
(`NO_COLOR` turns colour off), one plain line —
`toolu-runner: installed <version> (checksum ok)` — when piped.

Pass `--check` to print the plan (including which directory it would
use, the `SHA256SUMS` it verifies against and the setup options) and exit
without downloading anything. `--service` also installs
and starts the static single-slot unit shipped in the tarball, but only
when the binary lands at `/usr/local/bin/toolu-runner` — that path is
hardcoded in those units, so anywhere else the unit is skipped with a
warning; use `toolu-runner install-service` instead (it writes the machine
service for the binary's real path, and replaces those static units when
they run nothing — see *Running as a service*).

Prebuilt for **macOS** (arm64, x86_64) and **Linux** (x86_64, arm64). The
Linux binaries are built on Debian 12 (glibc 2.36), so any Debian 12 / Ubuntu
24.04 or newer host runs them; today's builds reference nothing past glibc
2.34, which covers Ubuntu 22.04 too.

## Quick start

```sh
# Guided onboarding: checks this machine, asks for the GitHub target
# and credentials (every question has a flag), reviews, registers,
# installs the background service, and confirms the runner is online.
toolu-runner setup

# The same result unattended (no terminal, no prompts):
TOOLU_RUNNER_TOKEN=<pat> toolu-runner setup --source github --org acme --auth token --yes

# Or let toolu.sh route jobs to this machine (no GitHub credential on it):
# type the code setup shows at console.toolu.sh/pair ...
toolu-runner setup --source toolu
# ... or, unattended, redeem a single-use enrollment token from the console
# (or TOOLU_RUNNER_ENROLL_TOKEN=<token> with --source toolu: out of shell history;
# the variable alone never switches a setup to toolu.sh)
toolu-runner setup --source toolu --enroll-token <token>
```

A toolu.sh machine stores only its host token (0600, never logged) under
`~/.toolu-runner/toolu/<org>/`, runs under the same machine service, and
takes jobs whose workflow says `runs-on: [toolu-self-hosted]`. Re-running
setup refreshes its labels and version instead of pairing again; `status`,
`doctor` and `remove` (which revokes it on toolu.sh first) understand it.

Or by hand:

```sh
# 1. Register — the repo is inferred from the cwd git remote `origin`
cd my-repo && toolu-runner register

# 2. Run the listener — stays online, re-registering after each job
toolu-runner run
```

No flags, no PAT to craft by hand: on an interactive terminal with no
token available, `register` runs GitHub's OAuth **device flow inline** —
enter a one-time code in your browser, and the minted token lands in a
0600 file under the runner home (set `TOOLU_RUNNER_KEYRING=1` to use the
OS keyring instead — note macOS re-prompts whenever the binary changes).
One login per host covers every repo: the token store is shared, at the
runner-home root. Until the built-in OAuth App ships, the device flow
needs your own App's client id — set `TOOLU_RUNNER_CLIENT_ID`, or run
`toolu-runner login --client-id <id>` once.

`run` stays online by default: after each job it re-mints a fresh JIT
config with your stored `login` token (or `TOOLU_RUNNER_TOKEN`, or — for a
`register --app` registration — a GitHub App installation token) and keeps
listening, so one
invocation survives many jobs, idle gaps, and re-dispatches — no manual
re-register. Pass `--once` to run a single job and exit. To keep every
registration running across reboots and crashes, `toolu-runner
install-service` installs **one service for the machine** — a launchd
(macOS) agent or a systemd (Linux) user/system unit running `toolu-runner run --all` — see
*Running as a service* under [Configuration](#configuration).

For autoscalers and throwaway hosts, `register --ephemeral` records a
one-job registration: `run` takes exactly one job (any conclusion), never
re-mints, then unregisters the runner on GitHub and deletes the local
`config.toml` / `credentials.json` (`_diag/` is kept). With a token
(`TOOLU_RUNNER_TOKEN` or a stored `login`) the removal is verified with the
same call `remove` uses; without one, toolu relies on GitHub removing a used
JIT runner itself and says the removal was not verified. Shutting down before
a job keeps the registration for the next `run`. If the unregister call fails,
the registration is kept and `run` exits 2 — retry with `toolu-runner remove`.
The machine service runs ephemeral registrations too: after the one job the
registration deletes itself and the service keeps running the others.
`install-service --config` (a unit for one registration) refuses one, since
that unit would restart forever.

| Mode | Jobs per process | After the job |
|---|---|---|
| `run` | unlimited | re-mint a fresh JIT config, keep polling |
| `run --once` | 1 | exit; the spent registration stays on disk |
| `run` on a `register --ephemeral` registration | 1 | unregister + delete the local registration |
| `run --all` (the machine service) | unlimited, one loop per registration | each registration as above; a fatal error stops only its loop |
| `boot` (container image) | 1 | exit; nothing was persisted |

Each registration gets its own directory —
`~/.toolu-runner/runners/<owner>/<repo>/` for a repository,
`~/.toolu-runner/orgs/<org>/` for an organization (config, credentials,
lock, logs) — so one machine holds runners for many repos and several orgs,
and jobs for *different* registrations run **concurrently**: the single-job
lock is per registration.

### Headless / GHES

Everything can also be explicit — for scripts, org-level runners, and
GitHub Enterprise:

```sh
# explicit repo or org URL (org runners always need --url)
toolu-runner register --url https://github.com/owner/repo

# explicit credential: a PAT with admin rights on the repo/org — flag or env
toolu-runner register --url https://github.com/owner/repo --token <pat>
TOOLU_RUNNER_TOKEN=<pat> toolu-runner register

# or a GitHub App saved by `create-app` and installed on the repo/org:
# no personal token at registration or at any later re-mint
toolu-runner register --app --url https://github.com/acme --runner-group mobile-ci

# GHES: register an OAuth App on the instance, log in against it,
# then register with an explicit --url (inference is github.com-only)
toolu-runner login ghes.example.com --client-id <id>
toolu-runner register --url https://ghes.example.com/owner/repo
```

Bearer resolution for `register`: `--app` > `--token` > `TOOLU_RUNNER_TOKEN`
> the GitHub App in `github-app.json` when it belongs to the URL's host >
stored `login` token > inline device flow (interactive terminals only —
non-interactive runs fail, listing the token options).

#### Registering with a GitHub App

`register --app` (or `register` with no token given, once `create-app` has
saved `github-app.json` for that host) authenticates as the App: it signs an
App JWT with the saved private key, finds the App's installation on the
repository (`GET /repos/{owner}/{repo}/installation`) or organization
(`GET /orgs/{org}/installation`), mints an installation token, and calls
`generate-jitconfig` with it — `/repos/{owner}/{repo}/…` for a repository URL,
`/orgs/{org}/…` for an organization URL. The registration records
`[auth] kind = "app"` with the App file and the installation id, so every
re-mint after a job mints its own installation token (kept in memory and
reused for up to 50 minutes, never written to disk): the runner needs no
stored login and no `TOOLU_RUNNER_TOKEN`. The App needs Administration
(read and write) on a repository, or Self-hosted runners (read and write)
on an organization: `create-app` requests the first for a personal App and
the second (`organization_self_hosted_runners: write`) for an `--org` App. For an organization URL, `--runner-group` takes a group
name (resolved through the org's runner-group list and recorded by id) or a
numeric id.

When the App is not installed on the target, `register` fails naming the
accounts it is installed on and its install URL. During `run`, GitHub
rejecting the App (a revoked key, a deleted App, a removed installation: 401
or 404) ends the loop with the fix; a 429 or 5xx backs off and retries. The
private key, the minted tokens and the App JWTs are registered with the
secret masker, so they never reach `_diag` logs. A token registration writes
no `[auth]` section and behaves exactly as before. `remove` still takes a
personal token (or `--skip-unregister`) for an App registration. When the App
was picked automatically (no `--app`), its errors say so: pass `--token` /
`TOOLU_RUNNER_TOKEN`, or move `github-app.json` aside, to register with a
personal token instead. `doctor` checks a persistent App registration's App
file (an `app` row).

`login` / `logout` manage the stored token per host — they take a
hostname (default `github.com`), not `--config`. `status` prints local
state — including login — without touching the network. `remove`
unregisters. `check` is the network preflight and `doctor` the
whole-machine health check (both below).
`boot` is the flag-less one-shot mode of the container image and `create-app`
the GitHub App onboarding: create (or import) and install (also below). That's the whole CLI.

### Preflight: `toolu-runner check`

`check` answers "can this host run jobs?" — the equivalent of the official
runner's `run.sh --check`. It is read-only: no lock, no registration, no
session, no credentials sent.

```sh
toolu-runner check                                 # the resolved registration, else github.com
toolu-runner check --url https://ghes.example.com  # a GHES instance, before registering
toolu-runner check --timeout 30                    # per-probe bound in seconds (1–300, default 10)
```

- **Network (required):** `api`, `codeload`, `pipelines`, `broker`, `token`
  and `results` — a registration's own JIT endpoints (`ServerUrl`,
  `ServerUrlV2`, the OAuth `AuthorizationUrl`) when one resolves, else the
  official runner's defaults for github.com / GHE.com
  (`api.github.com`, `codeload.github.com/_ping`,
  `*.actions.githubusercontent.com` health paths) or GHES (`/api/v3`,
  `/_services/{pipelines,vstoken}/_apis/health`; no broker or Results
  service on GHES). Any HTTP answer counts as reachable — a broker or token
  URL legitimately answers 401/404 without credentials — except
  `407 Proxy Authentication Required`. Failures are classified (`timeout`,
  `dns`, `tls`, `connect`, `proxy`) with a hint.
- **Network (optional):** `node-dist` (`nodejs.org`, where the runner
  downloads Node for JavaScript actions; musl builds come from github.com
  release downloads).
- **Tools (optional — only ever warn):** `git --version`, `docker version`
  (client, daemon, and the endpoint the runner uses: `DOCKER_HOST`, else
  `unix:///var/run/docker.sock`), `node --version` plus the runtimes cached
  under the registration's `data_dir/node/`.
- **Proxy / TLS trust:** the variables the runner's HTTP clients honour —
  `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY` (upper case before
  lower; `REQUEST_METHOD` set disables them all), `SSL_CERT_FILE` /
  `SSL_CERT_DIR` (when set they **replace** the platform trust store) with
  the loaded anchor count, and `NODE_EXTRA_CA_CERTS`. The runner's HTTP
  clients also always trust the compiled-in Mozilla root set
  (webpki-roots), so these variables can **add** a private or intercepting
  CA but cannot narrow trust. With a registration whose `config.toml` has a
  `[network]` section, the probes use it and a `[network]` line shows the
  effective proxy (redacted), `no_proxy` count and `ca_bundle` certificate
  count — see [Proxy and custom CA](#proxy-and-custom-ca); an unusable
  `ca_bundle` or proxy URL exits `2` before probing. Proxy and `DOCKER_HOST`
  credentials print as `***`.

| Exit | Meaning |
|---|---|
| `0` | Every applicable required endpoint answered (optional warnings allowed). |
| `1` | At least one required endpoint failed. |
| `2` | Usage or configuration error before probing (bad `--url`/`--timeout`, an unloadable `--config` or JIT config, an unusable `[network]`). |

### Health: `toolu-runner doctor`

`doctor` answers "is this machine serving my repos, and how do I fix what
isn't?". It checks every registration under the runner home (or one with
`--config`) and prints the fix command under each `WARN` / `FAIL` row that
has one.

```sh
toolu-runner doctor                     # every registration under TOOLU_RUNNER_HOME
toolu-runner doctor --config <PATH>     # one registration
toolu-runner doctor --offline           # local checks only: no network request at all
```

- **install:** the version (and the GitHub runner version it is compatible
  with) and whether a newer toolu-runner release exists. The fix is the
  install one-liner. A failed release check is `SKIP`, never a problem.
- **host:** the platform, a warning when running as root, and the free disk
  at the runner home and every work dir: `FAIL` below 1 GiB, `WARN` below
  5 GiB. On Linux with an installed unit, it also checks whether user
  lingering is on, because systemd user units only start at boot when it is.
- **github auth (per host):** the bearer `run` re-mints with after every
  job. That is `TOOLU_RUNNER_TOKEN`, else the stored `login` token. It is
  verified with one authenticated `GET <api>/user`, the only request that
  carries a credential. No bearer, an unreadable token store, or a bearer
  GitHub rejects is a `FAIL` when a persistent token registration needs it,
  and a `WARN` or `INFO` otherwise (a registration with `[auth] kind = "app"`
  re-mints with its GitHub App and needs no personal token). The token is
  masked in all output.
- **registrations:** for each registration:
  - `config.toml`, `credentials.json` (warns when its group or others can
    read it) and the JIT config all load.
  - Whether a `run` is serving it. This is read from the `.lock` body; the
    lock itself is never taken. A stopped persistent registration is a
    `FAIL`.
  - Its supervisor unit is installed and active.
  - Any pending `remove`.
  - The last job's conclusion, from the job journal.
- **network (per host):** the same endpoint, tool, proxy and TLS lines as
  `check`, using the registration's own JIT endpoints. If its JIT config
  does not decode, the host's generic endpoints are probed instead.

`doctor` is read-only. It never takes a lock, writes a file, opens a session
or registers.

| Exit | Meaning |
|---|---|
| `0` | No `FAIL` row (warnings allowed). |
| `1` | At least one `FAIL` row. |
| `2` | Usage error before checking (a `--config` path that does not exist). |

### Container one-shot: `toolu-runner boot`

`boot` takes no flags. It is the `ENTRYPOINT` of the container image, driven
entirely by the environment (a JIT config in argv would leak into provider
dashboards): `TOOLU_JITCONFIG` (required) is GitHub's `encoded_jit_config`
from `generate-jitconfig`, verbatim; `TOOLU_DEADLINE` (optional) is an
epoch-milliseconds decimal string, a self-timeout backstop (missing or
unparseable: no watchdog). It runs exactly one job and persists nothing — no
`config.toml`, `credentials.json`, `.lock` or re-mint. See the
[container image](docs/container-image.md).

| Exit | Meaning |
|---|---|
| `0` | The job succeeded or was skipped, or the runner shut down (signal or session end) before acquiring one. |
| `1` | The job failed or was cancelled, or the listener errored (auth, network, protocol). |
| `2` | Environment or config error before polling (missing `TOOLU_JITCONFIG`, unparseable JIT envelope). |
| `5` | A session conflict outlasted the bounded retry. |
| `124` | The `TOOLU_DEADLINE` watchdog fired: already past at start (exits before any network call), or mid-job (graceful cancel, then a hard exit after a 30 s grace period). |

### toolu.sh machines

A machine paired with a [toolu.sh](https://toolu.sh) organization runs the
jobs toolu.sh routes to it: workflows opt in with
`runs-on: [toolu-self-hosted]` (plus any labels the machine reports), and
toolu.sh holds the GitHub credentials and mints each job's JIT config. The
pairing itself is `toolu-runner setup`'s toolu.sh branch; it writes a
registration under `~/.toolu-runner/toolu/<org>/` with
`[provider] kind = "toolu"` and the machine's host token (0600).

For that registration, `run` (and the machine service's `run --all`) polls
toolu.sh instead of GitHub, one job at a time: list the queue, claim a job
(losing a race to another machine is normal — it moves on), run the claimed
JIT config once, renew the claim's lease while the job runs, and report how
it ended. A network outage or a 5xx backs off (1 s → 60 s) and resumes;
SIGINT/SIGTERM cancels a running job gracefully and still reports it. If
toolu.sh answers 401 — the machine was revoked in the console — a running job
is cancelled and `run` exits 2, asking you to pair again with
`toolu-runner setup`. `TOOLU_RUNNER_POLL_INTERVAL_MS` sets the wait after an
empty queue (default 5 s).

### GitHub App: `toolu-runner create-app`

`create-app` creates the runner's GitHub App through GitHub's App Manifest
flow, then waits for you to install it. One App serves every machine of an
account, so it is named after the account: `toolu-runner-<org>` with
`--org`, `toolu-runner-<login>` with `--account <login>`, else
`toolu-runner-<hostname>`, cut to GitHub's 34-character limit (GitHub's form
lets you rename it; App names are unique across GitHub).

1. **Target.** Without `--org` the App belongs to you and gets
   `administration: write` (repository runners). `create-app --org acme`
   first checks `GET /orgs/acme` (a typo exits 2 before anything else), then
   posts the manifest to `github.com/organizations/acme/settings/apps/new`,
   so the App is owned by the org and gets
   `organization_self_hosted_runners: write` (organization runners; GitHub
   needs no Administration permission at that scope). Only org owners (and
   the org's GitHub App managers) can create it: GitHub refuses anyone else
   and never redirects back, so the browser step times out after 5 minutes
   naming the token route (`register --token …`).
2. **Create** (first browser trip). A loopback callback on `127.0.0.1`
   serves the prefilled form (`--no-browser` prints the URL instead); the
   App is private and has no webhook. As soon as GitHub returns the App,
   its credentials are saved to `github-app.json` (0600) at the runner-home
   root: the App now exists on GitHub and its key can't be fetched again.
3. **Install** (second browser trip). `create-app` prints and opens the
   install link,
   `github.com/apps/<slug>/installations/new/permissions?target_id=<owner id>`
   (it skips the account picker), and polls `GET /app/installations` with
   the App JWT every 5 seconds until the App is installed on its owner. On a
   terminal, `o` + Enter opens the link again and `c` + Enter copies it (OSC
   52); Ctrl-C cancels (exit 130). After 10 minutes a terminal asks Keep
   waiting / Open the link again / Cancel; without a terminal `create-app`
   exits 1. It then records `installation_id` in `github-app.json` and prints
   `Installed · installation <id> · all repositories` (or the selected
   repository count). Then `register --app` (see
   [Registering with a GitHub App](#registering-with-a-github-app)).

**Over SSH, with no browser on the machine** (#205, picked automatically
over SSH or with no display; `--headless` / `--no-headless` override the
guess). Steps 2–3 run from another machine's browser. `create-app` prints
`This machine has no browser. Open this link on your laptop:` and a
`https://get.toolu.sh/gh/app#…` link: the page reads the manifest from the
URL fragment (never sent to a server) and posts it to GitHub, whose redirect
goes to `get.toolu.sh/gh/callback`. The machine keeps a random 244-bit secret,
prints only its SHA-256 as the round trip's `state`, and polls
`POST get.toolu.sh/gh/claim` with the secret, so the relay hands the code
to this machine only, once. While it waits, `p` + Enter lets you paste the
code the callback page shows (the relay can't be reached, say) and `c` +
Enter copies the link. Anyone holding the link could plant a code for an
App of their own, so the App GitHub returns must carry the requested name
(and owner, when `--org`/`--account` names it): otherwise it is refused,
nothing is saved, and the message names the App to delete. If the code was
already used, `create-app` names where to delete the App and start again.
Under `--no-relay` the machine never contacts the relay (your laptop still
loads the `get.toolu.sh/gh/app` form page): GitHub redirects to
`http://127.0.0.1:<port>/cb`, which fails to load on your laptop; paste that
address-bar URL (`p` + Enter), and the machine checks its `state` (a URL
from another run prints `✕ That URL is from a different setup run. Nothing
was exchanged.`). `--no-relay` needs a terminal. After 15 minutes without a
code `create-app` exits 1. A default App name is cut to GitHub's
34-character limit (a longer `--name` exits 2 before any request), so the
name check never meets a rename GitHub forced. `setup` takes the same three
flags, reads the paste from its own terminal (`/dev/tty`, even when stdin is
the install script), and refuses to create a personal App for repositories
an organization owns (it could not be installed there): use `--org` or
`--auth token`.

**One App, many machines.** `create-app --import <github-app.json>`, or
`--app-id <id> --app-key <pem file>`, reuses an existing App instead of
creating another: the key is proven with an App JWT call (`GET /app`)
before anything is saved, a key that doesn't match exits 2 and writes
nothing, and on success it prints `Key matches App <id> · installed on
<account>` (or waits for the installation as above). Re-importing the saved
`github-app.json` resumes a cancelled or timed-out wait.

github.com only — another `--host` is rejected before any browser or network
call — and it refuses to overwrite a saved App unless you pass `--force`.
Exit codes: `0` installed, `1` unattended wait timed out, `2` usage, auth or
config error, `130` cancelled.

### CLI flags reference

| Flag | Command | What it does |
|---|---|---|
| `--url <URL>` | `register` | Repository or organization URL. Optional: absent, the repo is inferred from the cwd git remote `origin` (github.com only); org runners and GHES need it. |
| `--token <TOKEN>` | `register`, `remove` | GitHub token with repository Administration or organization Self-hosted runners write access. Resolution: flag > `TOOLU_RUNNER_TOKEN` > (`register` only) the saved GitHub App for the host > stored `login` token; used only for the registration / unregister call, never at job runtime. |
| `--app` | `register` | Authenticate with the GitHub App saved by `create-app` (an installation token); recorded as `[auth] kind = "app"` so every re-mint uses the App. Conflicts with `--token`. |
| `--name <NAME>` | `register` | Runner name (default: the machine hostname). |
| `--labels <LABELS>` | `register` | Comma-separated custom labels, **added** to the defaults `self-hosted`, `Linux`/`macOS`, `X64`/`ARM64` (GitHub's `RUNNER_OS`/`RUNNER_ARCH` spelling, as the official runner). Empty entries are ignored, case-duplicates kept once, at most 256 characters (UTF-16 units, as upstream counts them). |
| `--no-default-labels` | `register` | Register only the `--labels` values (which are then required). |
| `--ephemeral` | `register` | One-job registration: `run` takes one job, then unregisters and deletes it (see above). |
| `--work <DIR>` | `register` | Job workspace directory (default `~/.toolu-runner/_work`). |
| `--runner-group <NAME\|ID>` | `register` | Runner group for org registrations: a name (resolved through the org's runner-group list, recorded by id) or a numeric id. Repository runners only have the Default group — a name there warns and uses it. |
| `--replace` | `register` | Overwrite an existing registration with the same name. |
| `--once` | `run` | Run a single job, then exit. The default is to stay online — after each job `run` re-mints a fresh JIT config (with `TOOLU_RUNNER_TOKEN` / the stored `login` token, or the registration's GitHub App) and keeps listening. |
| `--all` | `run` | Supervise every registration under the runner home in one process (what the machine service runs); SIGHUP rescans. Conflicts with `--config` / `--once`. |
| `--print` / `--no-activate` / `--remove` | `install-service` | Print the unit without writing it (conflicts with `--remove`) / write it but don't activate / deactivate and delete it (idempotent). Without `--config` they act on the machine service. |
| `--force` | `remove` | Remove even while a `run` holds the job lock. Local state goes, but the lock file is kept and the GitHub unregister is skipped (that job is still reporting against the registration), so remove the runner on GitHub by hand afterwards. It does not cancel the job. Without it, a held lock writes a `.pending_remove` marker: when the holder is the machine service, `remove` asks it to stop just that runner and waits up to 30 s for an idle one; otherwise (or with a job in flight) it exits 2. |
| `--skip-unregister` | `remove` | Delete local state without calling GitHub (revoked token, deleted repo, no network); the runner stays registered there. Without it, `remove` with no token, or with a failed unregister, aborts before deleting anything. |
| `--url <URL>` | `check` | Probe this GitHub host's endpoints instead of the resolved registration's (conflicts with `--config`). |
| `--timeout <SECS>` | `check`, `doctor` | Per-probe bound, 1–300 seconds (default 10). |
| `--offline` | `doctor` | Make no network request; the update and network rows print `SKIP`, and a present token is reported unverified. |
| `--client-id <ID>` | `login` | OAuth App client id for the device flow (fallback: `TOOLU_RUNNER_CLIENT_ID`). Needed until the built-in github.com App ships; always needed for GHES. |
| `--name <NAME>` | `create-app` | GitHub App name (default `toolu-runner-<org>`, `toolu-runner-<account>`, else `toolu-runner-<hostname>`). It must be unique across GitHub: a duplicate makes the browser flow time out. |
| `--org <ORG>` | `create-app` | Create the App owned by this organization, with `organization_self_hosted_runners: write` (org runners); the org is checked on GitHub first. With `--import`, the installation is looked for on this org. |
| `--account <LOGIN>` | `create-app` | Your personal login: names a personal App `toolu-runner-<LOGIN>`. Conflicts with `--org`. |
| `--import <FILE>` | `create-app` | Reuse an App from a `github-app.json`; the key is proven with GitHub before anything is saved. |
| `--app-id <ID>` / `--app-key <PEM_FILE>` | `create-app` | Reuse an App by id and private-key file (both required together); proven like `--import`. |
| `--host <HOST>` | `create-app` | GitHub host to create the App on (default `github.com`, the only value accepted this release). |
| `--no-browser` | `create-app` | Print the URLs (the manifest form, the install link) instead of launching a browser. |
| `--force` | `create-app` | Overwrite an already saved App (`github-app.json` at the runner-home root). |
| `--headless` / `--no-headless` | `create-app`, `setup` | Create the App from another machine's browser through the get.toolu.sh relay link (default over SSH or with no display), or force this machine's browser and the loopback callback. |
| `--no-relay` | `create-app`, `setup` | Headless without the relay: paste the address-bar URL GitHub redirects to (`127.0.0.1/cb?code=…&state=…`); its `state` is checked. Needs a terminal. |

`register`, `run`, `remove`, `status`, `install-service`, and `check`
also take `--config <FILE>`; when omitted, the config resolves to the cwd-inferred
`runners/<owner>/<repo>/config.toml` under the runner home, else the
sole existing registration (`install-service` without it installs the
machine service instead). `doctor --config` narrows its report to that one
registration; without it, `doctor` checks them all. Every command documents itself in full:
`toolu-runner <command> --help`.

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
        R->>RS: complete job (conclusion, outputs, step results, environment URL)
        R->>GH: delete session, re-mint JIT config
        Note over R: authenticate again with the new JIT config
        R->>GH: create next broker session
    end
```

One process, one job per registration. The single-job guarantee is an
`fs2` file lock on the registration's own `.lock` (under
`~/.toolu-runner/runners/<owner>/<repo>/`) whose body carries the
holder's PID — a second `run` for the same repo reads it, prints the
PID, and exits `2`. Stale locks (dead PID, mtime > 5 min) are reclaimed
automatically. Different registrations lock independently, so their jobs
run concurrently on one machine. In `offline` / `accelerated` service
modes each job's local cache server binds `service_bind` on an ephemeral
port, so concurrent registrations sharing one `service_bind` never collide.

`run --all` (what the machine service runs) drives one such loop per
registration in a single process, each holding its own `.lock`. It holds
`<home>/supervisor.lock` (a second `run --all` on the same home exits `2`),
and on `SIGHUP` rescans: a new registration starts polling, one whose removal
is pending is drained (it stops while idle; a running job always finishes
first), and a stopped one restarts. A fatal error — a re-mint GitHub
rejects, a session conflict, a corrupt config — stops only that loop and is
recorded in its `.stopped` file, which `status` and `doctor` show. A
registration another process still holds (an older unit shutting down, a
foreground `run`) is retried every 5 s, never driven twice. The process
exits `0` on `SIGINT`/`SIGTERM` and never with a registration's exit code.

`SIGINT`/`SIGTERM` are bridged to a `CancellationToken` that the poll
loop, the renewal task, and the in-flight job all observe. Nothing is
left orphaned.

A session conflict (GitHub answers `409` while another live session holds the
registration) is retried every 30 s, up to 8 attempts. If it outlasts that,
`run` exits `5`, with or without `--once`, and a per-registration systemd
unit (`install-service --config`) does not restart on it; under `run --all` it
stops only that registration's loop. `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS`
overrides the 30 s wait for `run` and `boot`.

### What runs

| | |
|---|---|
| **Steps** | `run:` shell, `uses:` Node.js actions (runtime auto-downloaded + cached, musl/Alpine aware, node24 migration, offline preseed), composite actions, Linux Docker actions (`runs.using: docker` and `uses: docker://`) |
| **Workflows** | matrices, `needs:` job graphs and job outputs, reusable workflows, `if:` conditions, `timeout-minutes`, `working-directory`, `defaults.run` |
| **Expressions** | `${{ }}` with signed/radix numbers, reference equality, lazy `case()` and `format()`, insertion-ordered pretty `toJSON`, G15 number rendering, and `.*` / `[*]` filters; unknown named values fail visibly |
| **Services** | artifacts, cache, and OIDC forwarded to GitHub by default; local cache in `accelerated` mode; legacy v1 cache only in `offline` mode |
| **Safety** | secret masking across logs, stdout, and the journal; strict-mode clippy (no `unwrap`, no `panic`, no `unsafe`) |

Remote `uses:` actions are resolved with the acquired job's GitHub service,
then cached by the server-resolved revision. On older servers without action
resolution, the runner uses that job's GitHub API host and token. Archive
credentials are masked and are not sent across a redirect to another origin.

For top-level `run:` steps, the runner reads ordered `defaults.run.shell` and
`defaults.run.working-directory` from the acquired job. A job `run` mapping
replaces the workflow `run` mapping; an explicit step value wins. Relative
directories resolve from that job's workspace, and a missing directory fails
the step with its path in the diagnostic. Composite steps keep their own scope.

Each launched shell, Node action stage (pre/main/post), and nested composite
shell receives `GITHUB_ACTIONS=true`. The runner supplies `CI=true` only when
`CI` is absent from the child environment and the runner environment. Existing
workflow, job, action-step, and runner values such as `CI=false` or an empty
`CI` are preserved. Linux job-container processes follow the same rule, with a
container declaration supplying the fallback when no job or step `CI` is present.
Docker action containers follow this environment rule too; see [test coverage](docs/test-coverage.md)
for measured platforms and live parity status.

Every host step process — shells, Node action pre/main/post, composite shells
and job hooks — starts with a per-job `RUNNER_TRACKING_ID=github_<uuid>`. Once
the job's steps, posts and containers are done, the runner logs `Cleaning up
orphan processes` and sends `SIGKILL` to every other process whose environment
still carries that id — or that was started during the job inside a tracked
step's own process group (every host step leads one) — logging `Terminate
orphan process: pid (N) (name)` for each. Background services started by one step therefore keep running for later
steps, but `nohup`, `setsid` and other detached leftovers do not outlive the
job. As with the official runner, a workflow value wins: `env:
RUNNER_TRACKING_ID: ''` (or any other value) on a step opts that step's
processes out, and the job variable `process.clean=false` disables tagging and
cleanup entirely. The sweep is bounded at 15 seconds and never changes the job
result. Linux reads `/proc/<pid>/environ` and macOS uses `KERN_PROCARGS2`.
Both see only the environment a process was started with, and only for
processes the runner's user may inspect. With SIP on, macOS shows no
environment at all for Apple platform binaries (`sleep`, `sh`, `bash`,
`/usr/bin/python3`, …); the process-group match covers those while they stay in
the step's group, but one that detaches into its own session or group
(`setsid`/`setpgid`) is not found there. Processes started through `sudo` or as another user, and an
`env -i` process that left the step's group, are not found either. Processes inside Docker
containers are never tagged; container teardown owns them. A runner crash, or
the `boot` deadline watchdog's hard exit, skips the sweep.

Steps export `GITHUB_ACTION` from the acquired action name (including generated
`__run`, `__run_2`, and `__owner_repo` names). Remote repository actions also
export `GITHUB_ACTION_REPOSITORY` and `GITHUB_ACTION_REF`; run, local, and
registry-image actions receive empty values for those two fields. Composite
children inherit the enclosing action name, scope repository/ref to their own
handler, and restore parent metadata on return. Deferred posts retain the
originating action identity. `RUNNER_ENVIRONMENT` and `${{ runner.environment }}`
are always `self-hosted`. See [metadata evidence](docs/test-coverage.md#action-metadata-issue-85)
for the measured host/container paths and unverified live lanes.

Workflow and job `env:` mappings from the acquired message are evaluated in
order. A job layer can read the previous layer through `env`; step `env` wins
for that step in both expressions and child processes. `GITHUB_ENV` updates
become visible to later steps and remain separate from temporary step overrides.
Malformed environment tokens fail setup before steps start. See
[test coverage](docs/test-coverage.md#workflow-and-job-environment-issue-69) for
measured replay coverage and pending live parity lanes.

`GITHUB_ENV`, `GITHUB_OUTPUT` and `GITHUB_STATE` are parsed like the official
Linux/macOS runner:

- Entries are `NAME=VALUE` or a `NAME<<DELIMITER` heredoc, chosen by whichever
  of `=` and `<<` comes first.
- Lines end at LF only, so a CR written by a CRLF tool stays in the value.
- A malformed file fails the step that wrote it, after its exit code is
  logged. Malformed means:
  - a heredoc without its closing delimiter, or whose last line has no
    newline;
  - a line with neither `=` nor `<<`, including a lone CR or blank spaces;
  - an empty heredoc name or delimiter;
  - an empty `=VALUE` name. The official runner fails this too, but with a
    .NET argument error; toolu reports `Invalid format '=VALUE'. Name must
    not be empty`.
- The failing step reports two masked errors, `Unable to process file command
  'env' successfully.` and the reason, such as `Invalid format 'text'`.
- Entries before the bad line still apply. The other file commands are
  processed independently. `continue-on-error`, `always()` and post steps
  behave as for any other step failure.
- Setting `NODE_OPTIONS` through `GITHUB_ENV` is ignored with an error
  annotation but does not fail the step.

See [the #96 test map](docs/test-coverage.md#strict-file-commands-96).

Job `outputs:` arrive in the acquired job message and resolve after main and
post steps finish. Empty values are omitted. Values matching a registered
secret mask are omitted with a warning; safe values reach downstream jobs as
`needs.<job>.outputs.<name>`.

`::error`, `::warning`, and `::notice` commands from shell and composite
steps become annotations on the corresponding step result in the Run Service
completion request. Their decoded title, source path and range are preserved;
secret values are masked before reporting. The runner's job-level connection
failure annotation remains attached to the job. The local payload replay is
covered by [the #82 test map](docs/test-coverage.md#run-service-annotations-82);
live Checks UI and GHES comparison require provisioned paired runners.

### Job containers

On Linux, `jobs.<id>.container` selects one container for shell steps,
Node action pre/main/post stages, and composite shell steps. The runner exposes
`job.container.id` and `job.container.network`, mounts workspace, temp,
file-command and tool-cache directories, and removes the container/network
before reporting job completion. Cancellation detaches the active exec; eligible
post steps retain the container, and final removal terminates remaining processes.
Jobs declaring a container on macOS or Windows fail before any step runs.

Use a local Linux Docker daemon over a Unix socket (`DOCKER_HOST`, or the active
Docker CLI context) and `forwarder` services mode. The image must provide `tail`
and the requested shell; the default container shell is `sh`. Node actions use
runner-provided Linux binaries chosen per container, like the official runner:
an Alpine image (its `/etc/*release` `ID` contains `alpine`) gets the musl build,
any other image the glibc build — see [Node.js runtimes](#nodejs-runtimes). A
runtime failure fails the step without falling back to the host.

Both image shorthand and the object form support evaluated `image`, registry
`credentials`, `env`, `ports`, `volumes`, and `options`. Options accept CPU/memory/
PID limits, user/group, hostname/domainname, capabilities, security options,
ulimits, `--privileged`, and `--read-only`; unsupported options fail explicitly.
The runner reserves name, network, entrypoint, environment and mount flags;
workflow volumes cannot cover `/github`. Registry credentials and multiline
workflow environment values travel through the Docker API. The image supplies
the base PATH, with `GITHUB_PATH` entries prepended.

Service-container and Docker-action integration, live GitHub.com comparison,
and GHES acceptance are **unverified**; see the
[#73 coverage map](docs/test-coverage.md#job-containers-73).

### Node.js runtimes

`runs.using: node12 | node16 | node20 | node24` (case-insensitive) is accepted;
`node12`/`node16` run as node20 and any other `node*` fails with the official
runner's message. Pinned releases match actions/runner v2.337.0: node20 →
`20.20.2`, node24 → `24.21.0`.

**node20 → node24 migration.** A node20 action's runtime follows the official
runner exactly. GitHub sends the phase as job variables
(`actions.runner.usenode24bydefault`, `actions.runner.requirenode24`,
`actions.runner.warnonnode20`); github.com currently sends `requirenode24`, so
node20 actions run on Node 24. Before that phase,
`FORCE_JAVASCRIPT_ACTIONS_TO_NODE24=true` opts in and, once node24 is the
default, `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true` opts out — set either in
the workflow/job/step `env` or the runner's own environment (the workflow value
wins). Both set true in the same place is a configuration error: the step warns
and the phase default applies. The "Complete job" step lists actions that were
forced to Node 24 or are still on Node 20, as the official runner does.

**Which build.** The host gets the nodejs.org build for its OS/CPU (Linux or
macOS, x64 or arm64). A musl host (no glibc loader, an `ld-musl` loader present)
and an Alpine job container get the musl build from
[`actions/alpine_nodejs`](https://github.com/actions/alpine_nodejs), which —
like the official runner — exists for x64 only; Alpine on arm64 fails the step
before the action starts. A downloaded runtime that the host is meant to run
must pass `node --version` before it is committed to the cache
(`data_dir/node/<version>`, `<version>-alpine` for musl).

**Offline / pre-seeded.** Set `TOOLU_RUNNER_NODE_DIR` (for `run`, `boot`, or the
service unit's environment) to a directory in the official runner's `externals/`
layout — `node20/bin/node`, `node24/bin/node`, `node20_alpine/bin/node`,
`node24_alpine/bin/node`; an official runner's `externals/` folder works as-is.
It is authoritative: toolu never downloads, a missing runtime fails the step
naming the expected path, and a host runtime must start and report the right
major. Job containers see it read-only at `/github/externals`, so keep real
files there rather than symlinks that point outside it.

**Upgrading.** The pins moved from 20.18.3 / 24.0.2, so the cache directory
names changed: the first Node action after the upgrade downloads
`node/20.20.2` or `node/24.21.0` (github.com's current flags select 24 for
node20 actions). Airgapped hosts that hand-seeded `data_dir/node/<version>`
must reseed those two versions (or switch to `TOOLU_RUNNER_NODE_DIR`); the old
`20.18.3` / `24.0.2` directories are no longer used and can be deleted.

An Alpine host running `gcompat` exposes a glibc loader shim, so it is treated
as glibc and the glibc build then fails its start check with a clear error. On
such a host, preseed the musl builds under the glibc names (`node20/`,
`node24/` in `TOOLU_RUNNER_NODE_DIR`): that is what a glibc host looks up, and
they pass the start check because they run there.

Coverage and unverified lanes: [#95 coverage map](docs/test-coverage.md#node-runtime-95).

### Workflow service containers

Linux runners start acquired `services:` declarations before user steps. Host
jobs connect through `localhost:${{ job.services.<id>.ports[<container-port>] }}`;
job-container steps connect using the service ID as a DNS alias. Each service
context contains its actual Docker `id`, shared `network`, and published `ports`.
Services remain available through post actions. Logs are added to the job log
before owned containers and networks are removed, including setup failures and
cancellation. User-provided volumes and unrelated Docker resources are preserved.

Image credentials, environment, ports, volumes and the documented restricted
container options are evaluated before creation. Health options include
`--health-cmd`, `--health-interval`, `--health-timeout`, `--health-retries`,
`--health-start-period`, `--health-start-interval` and `--no-healthcheck`.
Health durations accept Docker duration syntax (for example `500ms`, `0.5s`,
`1m30s`). A service without a health check starts without an application-readiness
guarantee. A starting health check is retried with 2–32-second backoff, up to
300 seconds per service; unhealthy or timed-out services fail the job before
steps. This explicit bound differs from the pinned official runner's wait until
health resolves or the job is cancelled. Nonempty service declarations fail
explicitly on macOS. Empty or disabled services require no Docker daemon.

See [the service evidence map](docs/test-coverage.md#service-containers-issue-74)
for verified real-Docker scenarios and outstanding live/reference/GHES evidence.

### Docker actions

On Linux, `uses: docker://image:tag` pulls and executes the image. Repository
and local actions with `runs.using: docker` may use `image: Dockerfile` or a
`docker://` image. Successful builds from immutable resolved remote revisions
are reused while their images exist; local actions build against current files.

Manifest `args` preserves argument boundaries and empty values. When absent,
`with.args` supplies shell-quoted arguments without executing a host shell.
Manifest `entrypoint` wins over `with.entrypoint`; step environment wins over
`runs.env` defaults. Inputs, GitHub context, runtime service variables and private
stage state are passed into the container. Conditional `pre-entrypoint` and
`post-entrypoint` stages use job-context `pre-if`/`post-if` (default `always()`);
local actions skip pre with a warning, matching the official runner. Posts drain
in reverse order and their failures affect the job result.

Actions mount `/github/workspace`, `/github/home`, `/github/workflow`,
`/github/file_commands`, runner temp and `/var/run/docker.sock`. They join the
job or service-only network when one exists. File commands and stdout workflow commands use the
normal runner handlers; command paths are translated across mounts. Each stage's
container and its anonymous volumes are removed after success, failure, timeout
or cancellation. A missing
image, failed build or invalid entrypoint fails the step. macOS and Windows
reject Docker actions explicitly.

See the [#75 evidence map](docs/test-coverage.md#docker-container-actions-75)
for real-Docker replay and the separately recorded live/reference limitations.

### Service modes

`[services] mode` decides where artifacts, cache, and OIDC go.

- **`forwarder`** (default) — the runner reads the real GitHub service
  URLs and runtime token out of the job message and injects them into
  step env for upstream artifact, cache and OIDC access. Cache v2 flag
  forwarding still has a [known compatibility gap](docs/known-bugs.md#current-limitations)
  that can prevent saves and restores.
- **`offline`** — starts only the local legacy v1 cache service and sets
  `ACTIONS_CACHE_URL` / `ACTIONS_RUNTIME_TOKEN`. It does not start the
  local artifact or OIDC implementations, or forward GitHub service URLs;
  it is not a complete airgapped replacement for GitHub Actions services.
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
`pull_request` job cannot poison a protected branch's cache. The
S3 cold-tier implementation exists in the cache crate, but the job runner
does not wire it up: setting `[cache.l2] enabled = true` does not mirror
chunks or manifests off-box.

```toml
[services]
mode = "accelerated"     # "forwarder" (default) | "offline" | "accelerated"
bind = "0.0.0.0"         # must not be loopback — docker-container BuildKit reaches it here

[cache]
max_bytes          = 107374182400   # 100 GiB local (L1) budget
entry_ttl_days     = 7              # matches GitHub
protected_branches = ["main", "master"]
chunk_avg_bytes    = 65536          # FastCDC target chunk size
fsync_chunks       = false          # per-chunk fsync before rename — see below

[workspace]
mode = "stable"            # "stable": reuse _work/<repo>/<repo> per repository (upstream layout); "fresh": new _work/<job-id> per job
gc_after_hours = 24        # prune fresh _work/<job-id> dirs older than this (0 = never)
stable_gc_after_hours = 0  # prune a stable workspace unused for this long (0 = never, as upstream)

[shadow]
enabled = false          # off by default; records would-hit/false-hit, never serves
```

**Workspaces.** By default every job for `owner/repo` runs in
`_work/<repo>/<repo>` (`GITHUB_WORKSPACE`; `RUNNER_WORKSPACE` is
`_work/<repo>`), the official runner's layout, so `actions/checkout` finds
the previous `.git` and fetches incrementally. The mapping lives in
`_work/_PipelineMapping/<owner>/<repo>/PipelineFolder.json`; a repository
with the same name under another owner gets `<repo>_2`. Each job holds an
exclusive lease on its workspace, so a second job for the same repository
(another registration sharing the work root) runs in a fresh `_work/<job-id>`
instead of sharing it. The job message's `workspace.clean` (`all` /
`resources` / `outputs`) is applied as upstream does. `mode = "fresh"` opts
out: every job gets a new `_work/<job-id>`, as before. GC only removes
workspaces this registration used, never one a live job holds (pre-#90
`_work/<job-id>` dirs keep the old age-only rule).

`fsync_chunks` (default `false`) skips the per-chunk `fsync` before a
chunk's atomic rename, trading it for save-path throughput. This does
**not** weaken chunk integrity: every chunk is BLAKE3-verified on
read regardless, so a torn chunk left by an unclean shutdown is never
served silently. The read that DETECTS the mismatch removes the
corrupt chunk, but still returns an error partway through the body —
the status and `Content-Length` were already sent, so that first read
looks to the client like an aborted/truncated download, not a clean
miss. Only the corrupt chunk's removal is immediate: every SUBSEQUENT
lookup is a clean cache miss, never a permanently poisoned entry.
The library can restore from L2 when a caller supplies one, but the job
runner currently uses local storage only. Set `fsync_chunks = true`
to restore the previous per-chunk fsync before rename.

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
listener subset in Rust, with a strict `sync protocol` → `async wire`
boundary that keeps the crypto and wire-format code testable without a
clock, a socket, or tokio.

| Subsystem | `actions/runner` | `toolu-runner` |
|---|---|---|
| JIT config parse + RSA + JWT | C# | `protocol::auth` *(sync, no I/O)* |
| Token exchange / session | C# | `wire::net` |
| Message poll loop | C# | `listener::job_lifecycle` |
| Run service (acquire/renew/complete) | C# | `wire::reporting::run_service` |
| Results service (Twirp) | C# | `wire::reporting::results_service` |
| Expression engine (`${{ }}`) | C# | `expressions` *(own crate)* |
| Step handlers | C# | `execution::execution::handlers` |
| Artifacts / OIDC | C# | `execution::execution::{artifacts,oidc}` |
| Content-addressed cache | C# | `cache` *(own crate)* |
| Secret masking | C# | `shared::SecretMasker` + tracing layer |
| Docker | C# | `execution::docker::action_container` *(bollard; Linux actions with image build/pull, mounts and cleanup)* |
| Node.js auto-download | C# | `execution::node::runtime` |

**Deliberately not ported:** OpenTelemetry, and any coupling to the
`yamless` orchestrator this code was extracted from.

**GHES** is supported over the V1 protocol (`connectionData` discovery,
timeline records); protocol version is auto-selected from the `--url`
host at `register` time.

## Configuration

<details>
<summary><code>~/.toolu-runner/runners/&lt;owner&gt;/&lt;repo&gt;/config.toml</code> (mode 0600)</summary>

```toml
runner_url   = "https://github.com/owner/repo"
runner_name  = "my-runner"
runner_id    = 12345
auth_token   = "ghs_..."
labels       = ["self-hosted", "Linux", "X64"]
runner_group = "Default"
ephemeral    = false                         # `register --ephemeral`

[runtime]
jit_config       = "<base64 blob from GH>"   # written by `register`
work_dir         = "~/.toolu-runner/_work"
data_dir         = "~/.toolu-runner/runners/owner/repo"
protocol_version = "v2"                      # "v1" for GHES

[services]
mode = "forwarder"   # "forwarder" (default) | "offline" | "accelerated"

[network]                                     # optional — see "Proxy and custom CA"
proxy     = "http://user:pass@proxy.corp:3128"
no_proxy  = ["internal.corp", ".svc.local", "10.0.0.0/8"]
ca_bundle = "~/certs/corp-root.pem"

[auth]                                        # only for `register --app`
kind            = "app"                       # absent = "token"
app_file        = "/home/me/.toolu-runner/github-app.json"
installation_id = 987654
```

`[cache]` configures local cache storage; `[workspace]` and `[shadow]`
apply in every services mode. These sections are optional and default as
shown — see [Cache acceleration](#cache-acceleration). `[cache.l2]` is
parsed and preserved but is not connected to production job execution.

Credentials live beside it in `credentials.json` (also 0600). Don't
hand-edit `jit_config` or `auth_token` — re-run `register --replace`.

</details>

<details>
<summary>Storage layout</summary>

```
~/.toolu-runner/                # runner home — override with $TOOLU_RUNNER_HOME
├── token-<host>.json           # `login` token (0600, default store; keyring opt-in) — shared by all repos
├── github-app.json             # saved GitHub App (0600), shared by registrations
├── _diag/                      # process diagnostics: runner.log.YYYY-MM-DD, daily rotation
├── _work/                      # job workspaces (shared default): <repo>/<repo>/ stable, <job-id>/ fresh
│   └── _PipelineMapping/       # <owner>/<repo>/PipelineFolder.json + workspace leases
├── supervisor.lock             # held by `run --all` (JSON: its pid) — `register`/`remove` signal it
├── config.toml                 # legacy single-slot registration (still loaded; never written by default)
├── orgs/<org>/                 # one directory per organization registration (same files as below)
├── toolu/<org>/                # toolu.sh registration + host-token credentials
└── runners/<owner>/<repo>/     # one directory per repo registration
    ├── config.toml             # registration + runtime config (0600)
    ├── credentials.json        # (0600)
    ├── .lock                   # per-registration single-job lock (JSON: pid, started_at, config_path; emptied on release)
    ├── .pending_remove         # written by `remove` while a run is in flight
    ├── .stopped                # why `run --all` stopped this registration (cleared when it restarts)
    └── _diag/
        └── jobs/               # per-job JSONL journals (newest 50)
```

Process diagnostics always use `~/.toolu-runner/_diag/`, even when
`TOOLU_RUNNER_HOME` changes the state root. Per-job journals use the
registration's `runtime.data_dir`.

`remove` deletes a registration's `config.toml`, `credentials.json`,
`.lock`, `.pending_remove` and `.stopped`; `_diag/` is kept, so job history
survives unregistration. Removing the last registration also deactivates and
deletes the machine service.

</details>

<details>
<summary>Environment variables</summary>

| Variable | Default | Description |
|---|---|---|
| `TOOLU_RUNNER_HOME` | `~/.toolu-runner` | State root: token/App stores, repo/org/toolu.sh registrations, default `_work/`; does not move process diagnostic logs. |
| `TOOLU_RUNNER_ENROLL_TOKEN` | — | Enrollment token for `setup` after toolu.sh is selected; fallback for `--enroll-token`, never selects the source itself. |
| `TOOLU_RUNNER_TOOLU_API_URL` | `https://console.toolu.sh` | API origin for toolu.sh pairing and enrollment. |
| `TOOLU_RUNNER_POLL_INTERVAL_MS` | `5000` | Wait after an empty toolu.sh queue. |
| `TOOLU_RUNNER_REMOVE_WAIT_MS` | `30000` | Maximum wait for a supervised registration to drain during removal. |
| `TOOLU_RUNNER_ONLINE_TIMEOUT_MS` | `60000` | How long `setup` waits for the new runner (or toolu.sh machine's first poll) to come online. |
| `TOOLU_RUNNER_TOKEN` | — | bearer for `register`, `remove` and `run`'s re-mint after every job (resolution: `--token` > env > (`register` only) saved App > stored `login` token; a `register --app` registration re-mints with its GitHub App instead); `doctor` verifies it. |
| `TOOLU_RUNNER_CLIENT_ID` | — | OAuth App client id for the device flow (`--client-id` fallback). |
| `TOOLU_RUNNER_KEYRING` | — | when set (non-empty, not `0`), opts the token store in to the OS keyring (default is a 0600 file — on macOS the keychain re-prompts whenever the binary's code signature changes, e.g. every rebuild). |
| `TOOLU_RUNNER_NO_KEYRING` | — | when set (non-empty, not `0`), forces the 0600-file token store and skips the OS keyring probe entirely; overrides `TOOLU_RUNNER_KEYRING` (headless hosts, CI, locked keyrings). |
| `TOOLU_RUNNER_NODE_DIR` | — | directory of pre-seeded Node.js runtimes in the official runner's `externals/` layout; authoritative, so the runner never downloads (`run`, `boot`, the service unit's environment). See [Node.js runtimes](#nodejs-runtimes). |
| `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS` | `30000` | wait between session-conflict retries, in milliseconds (a positive integer; the 8 attempts are fixed). Read by `run` and `boot`. |
| `TOOLU_JITCONFIG` | — | `boot` only: GitHub's `encoded_jit_config`, verbatim. Required. |
| `TOOLU_DEADLINE` | — | `boot` only: epoch milliseconds (decimal string) of the self-timeout backstop. Optional. |
| `TOOLU_RUNNER_GITHUB_API_URL` | `https://api.github.com` | GitHub REST API base `create-app` uses for the org check, the manifest conversion and the App calls (a mirror or test peer). |
| `TOOLU_RUNNER_INSTALL_WAIT_MS` | `600000` | how long `create-app` waits for the App's installation before asking (terminal) or exiting 1 (no terminal), in milliseconds. |
| `TOOLU_RUNNER_INSTALL_POLL_MS` | `5000` | how often `create-app` polls for the installation, in milliseconds. |
| `TOOLU_RUNNER_RELAY_URL` | `https://get.toolu.sh` | relay origin a headless `create-app` prints its form link under and claims the manifest code from (a test peer). |
| `TOOLU_RUNNER_RELEASES_URL` | the latest `Falconiere/toolu-ghrunner` release on `api.github.com` | release document `doctor`'s newer-release check reads (a mirror); credentials in it are masked. |
| `TOOLU_RUNNER_LOG` | `info` | tracing filter. Checked before `RUST_LOG`. Honoured only with `TOOLU_RUNNER_ALLOW_VERBOSE=1`; otherwise the runner always logs at `info`. |
| `RUST_LOG` | — | tracing filter (standard fallback). Same `TOOLU_RUNNER_ALLOW_VERBOSE` gate. |
| `TOOLU_RUNNER_ALLOW_VERBOSE` | — | `1` lets `TOOLU_RUNNER_LOG` / `RUST_LOG` take effect. Off by default so debug/trace output cannot leak secrets into the log file. |
| `DOCKER_HOST` | `unix:///var/run/docker.sock` | Docker daemon endpoint, read the same way the Docker CLI reads it — required for Colima, OrbStack, Podman and Rancher Desktop, which do not publish the default socket. `unix://`, `tcp://` and `http://` are supported; an `ssh://` or `https://` value is rejected with an error naming the endpoint rather than silently falling back. |
| `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` | — | proxy for the runner's HTTP clients when `config.toml` has no `[network] proxy` (also read in lower case; `REQUEST_METHOD` set disables them all). See [Proxy and custom CA](#proxy-and-custom-ca). |
| `SSL_CERT_FILE` / `SSL_CERT_DIR` | — | replace the platform trust store for the runner's HTTP clients (the compiled-in Mozilla roots stay trusted). |
| `PATH` | — | `install-service` bakes the installing shell's `PATH` into launchd and systemd units. |
| `TOOLU_RUNNER_REPO` | `Falconiere/toolu-ghrunner` | `install.sh` only — release source. |
| `TOOLU_RUNNER_INSTALL_DIR` | `/usr/local/bin` | `install.sh` only — preferred install dir when `--install-dir` is absent; still falls back to `~/.local/bin` when not writable. |
| `TOOLU_RUNNER_RELEASE_URL` | `https://github.com/$TOOLU_RUNNER_REPO/releases` | `install.sh` only — release base URL the tarball and `SHA256SUMS` are fetched from (`<base>/latest/download/…` or `<base>/download/<tag>/…`). |
| `TOOLU_DAEMON_*` | — | the separate `toolu-daemon` bin only (token file, vCPU/memory budget, image, bind, queue ceiling, runtime) — see the [VPS host runbook](docs/vps-host-runbook.md). |
| `HOME` / `USERPROFILE` | — | resolves `~/.toolu-runner/`. |
| `HOSTNAME` / `COMPUTERNAME` | `unknown` | names this host in the broker session's `owner_name` (`<host> (PID: n)`). The default runner name at `register` comes from the OS hostname, not these. |

`TOOLU_RUNNER_CONFIG` / `_WORK` / `_LABELS` are not recognized; use the
CLI flags. Job steps never inherit the runner's own
`TOOLU_RUNNER_*` variables.

</details>

<details>
<summary>Running as a service</summary>

`toolu-runner` never daemonizes itself — `run` blocks in the foreground and
loops. To keep runners up across crashes and reboots, `install-service`
installs **one service for the machine** that runs `toolu-runner run --all`,
which supervises every registration under the runner home. The supervisor
owns process lifetime; each registration's always-online loop owns that
registration's lifetime.

```sh
toolu-runner install-service                # migrate older units, write + activate the machine service
toolu-runner install-service --print        # print the unit (stdout) and what it would replace (stderr)
toolu-runner install-service --no-activate   # write it, print the load command
toolu-runner install-service --remove       # deactivate + delete the machine service (idempotent)
toolu-runner install-service --config <path> # a unit for that one registration instead
toolu-runner install-service --enable-linger  # unattended Linux user install when linger is off
```

Adding a registration needs no restart: `register` sends the running service
`SIGHUP` and the new runner starts polling ("the toolu-runner service (pid N)
picks it up now"); other registrations' jobs are not disturbed. Reload by
hand with `systemctl --user reload toolu-runner` (Linux user unit),
`systemctl reload toolu-runner` (Linux root system unit), or
`kill -HUP <pid>` — e.g. after fixing a registration `doctor` reports as
*stopped by the service*. `remove` stops only that registration (an idle one
within seconds; a running job finishes first) and, for the last registration,
deactivates and deletes the service.

**Migration.** `install-service` first finds older toolu units for this home:
per-registration units, the pre-machine-service legacy-root unit (systemd
`toolu-runner.service`, now the machine service's name), and the static
`install.sh --service` agent (`io.toolu-runner`). It deactivates and deletes
every one that runs a registration of this runner home, or whose config no
longer exists — before starting the machine service, so no registration is
ever driven twice (the per-registration `.lock` guarantees that anyway).
Stopping an old unit stops its runner like `systemctl stop` does: **a job it
is running is cancelled** (each one is named as it stops), so migrate
between jobs. A unit that cannot be deactivated (no user bus over SSH, say)
is kept and the install stops, naming the command that removes it. A machine
unit serving another runner home is never overwritten. A
unit running a config **outside** the runner home is left running, with a
warning naming the command that removes it. Root installations can replace a
static systemd unit in `/etc/systemd/system/` when it serves this home or a
missing config; non-root installations only report system units. A live
foreign system unit at the destination blocks the root install. `doctor`
fails a registration when two units
supervise.

**launchd (macOS)** — a user `LaunchAgent` labelled `sh.toolu.runner` at
`~/Library/LaunchAgents/`, with `KeepAlive` + `RunAtLoad`. Activated with
`launchctl bootstrap gui/<uid>` (falling back to legacy `launchctl load -w`);
stdout/stderr go to `<home>/_diag/service.{out,err}.log`.

The plist carries an `EnvironmentVariables` `PATH` — **the `PATH` of the shell
that ran `install-service`**, plus `/opt/homebrew/{bin,sbin}`,
`/usr/local/bin` and the system four if they were missing. Job steps inherit
the runner process's environment, and launchd otherwise hands an agent only
`/usr/bin:/bin:/usr/sbin:/sbin` — without this a workflow that runs fine from
your terminal fails under the supervisor with `command not found` for anything
Homebrew installed. It is baked in at install time, so **re-run
`install-service` after changing your `PATH`** (the command rewrites the
unit). `install-service --print` shows the exact value that will be used.
Systemd units also carry this PATH, so job steps see the tools available to
the installing shell. A customised `TOOLU_RUNNER_HOME` is pinned in user
units. Root system units pin `HOME` and `TOOLU_RUNNER_HOME` even for the
default home, since the system manager may not set `HOME`.

**systemd (Linux)** — a non-root install writes a user unit at
`~/.config/systemd/user/toolu-runner.service`, with `Restart=always` +
`RestartSec=5` and `ExecReload` sending `SIGHUP`. It activates via
`systemctl --user daemon-reload` and `systemctl --user enable --now
toolu-runner.service`; logs land in journald (`journalctl --user -u
toolu-runner`). On a headless host, the installer checks linger before writing
the unit. If linger is off, it asks “Keep running after you log out?” and
shows `loginctl enable-linger <user>`. `--enable-linger` or `--yes` answers Yes
for unattended installs. Choosing No installs a service that stops on logout
and prints a warning. If enabling linger fails, no unit is installed; run
`sudo loginctl enable-linger <user>` and retry. `--print` and `--remove` do not
change linger. After changing PATH, re-run `install-service` to update the
unit's environment.

As root, `install-service` writes `/etc/systemd/system/toolu-runner.service`,
uses `systemctl daemon-reload` and `systemctl enable --now`, and starts at boot
without linger. `--config <path>` uses the same user or system scope as the
machine unit. The root unit's environment pins the installing `HOME` and the
resolved runner home. Use `systemctl is-active toolu-runner` and
`journalctl -u toolu-runner` for the root unit.

For a local systemd parser and inherited PATH check, run
`bash scripts/test/headless_service_systemd.sh`. A real VPS check is
`scripts/test/headless_service_live.sh`: it requires a disposable, registered
non-root SSH target and a `workflow_dispatch` job on that runner with
`nonce` and `expected_path` inputs, `run-name: ${{ inputs.nonce }}`, a step
that compares `$PATH` to `expected_path`, and a log line
`HEADLESS_PATH=$PATH`. Set `TOOLU_HEADLESS_SSH`, `TOOLU_HEADLESS_REPO`,
`TOOLU_HEADLESS_WORKFLOW`, `TOOLU_HEADLESS_EXPECTED_PATH`, and
`TOOLU_HEADLESS_ALLOW_REBOOT=1`. The script installs the unit, reconnects
after logout and reboot, then dispatches and checks the workflow run. It
also checks a root system-scope install after the job, then removes that
root unit. It reports `UNVERIFIED` when the lane is unavailable.

**One registration** — `install-service --config <path>` keeps the
per-registration unit for scripted setups: `io.toolu.runner.<owner>.<repo>` /
`toolu-runner-<owner>-<repo>.service` for a repo,
`io.toolu.runner.orgs.<org>` / `toolu-runner-orgs-<org>.service` for an org,
`io.toolu.runner` / `toolu-runner-legacy.service` for the legacy root, each
running `run --config <path>` (systemd adds `RestartPreventExitStatus=5`, so a
session conflict that outlasted its retry stays down). Don't combine it with
the machine service for the same registration.

Only launchd and systemd are supported — any other platform errors, naming
those two as the targets.

The release tarball also ships **static single-slot** units under `scripts/`
(`io.toolu-runner.plist`, `toolu-runner.service`); `install.sh --service`
installs and starts them (systemd lands in `/etc/systemd/system/` and needs
root). Those predate the per-registration layout — use `install-service` on
any new host.

</details>

## Proxy and custom CA

Every runner HTTP client — the listener, JIT re-mint, `remove`, `check`,
`doctor` (its first registration's settings for the release check), action downloads, Node runtime downloads, the live-log WebSocket and the
accelerated cache's upstream — is built from one place (`crates/network`)
and honours the same settings. The local OIDC server and the S3 cold tier
build their clients the same way but are not wired into a production job
path yet. Nothing ever turns TLS verification off.

**Environment mode (no `[network] proxy`, the default).** The runner reads
`HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` exactly as reqwest
does: upper case before lower case, an unparseable scheme variable falls back
to `ALL_PROXY`, `REQUEST_METHOD` set disables them all, and `NO_PROXY`
entries match a domain and its subdomains, IPs and CIDRs, or `*`. TLS trusts
the OS store (`SSL_CERT_FILE` / `SSL_CERT_DIR` replace it) plus the
compiled-in Mozilla set. `boot`, `login`, `create-app` and the first
`register` always use this mode (no registration config exists for them).

**Config mode.** In the registration's `config.toml`:

```toml
[network]
proxy     = "http://user:pass@proxy.corp:3128"   # http:// or https://; used for http and https destinations
no_proxy  = ["internal.corp", ".svc.local"]      # NO_PROXY syntax per entry
ca_bundle = "~/certs/corp-root.pem"               # extra trusted CA(s), PEM
```

| Setting | Effect |
|---|---|
| `proxy` set | Replaces the proxy variables **and** env `NO_PROXY` for the runner's clients. `localhost`, `127.0.0.1` and `::1` are always bypassed (as the official runner does), on top of `no_proxy`. Credentials are sent as Basic auth. Percent-encode reserved characters in the user name and password: an unencoded `/`, `?` or `#` (anything that leaves an `@` outside the parsed credentials) is rejected; an unencoded `@` or `:` in the password is tolerated. |
| `no_proxy` | Used only with `proxy`. |
| `ca_bundle` | Works with or without `proxy`. **Adds** trust anchors; never removes the OS or Mozilla roots. Must be absolute after `~` expansion, a readable file, and contain at least one well-formed PEM certificate — anything else is an error naming the path, and `run` / `check` exit `2` before polling. |

`run` re-reads `[network]` at the start of each poll cycle, so an edit made
while the runner waits takes effect after the job it is waiting for (or on
restart); an edit that breaks it stops the runner (exit `2`) instead of
silently dropping the proxy or CA. `run` logs one line at startup and
whenever the effective settings change, `boot` once:
`network: proxy http://***@proxy.corp:3128 (config), no_proxy 2 entries,
ca_bundle /home/u/certs/corp-root.pem (1 certificates)`. In environment mode
the line names the proxy each scheme really uses, for example
`proxy https http://***@proxy.corp:3128 (env HTTPS_PROXY), http direct`.
An unknown key in `[network]` (a typo such as `ca_bundel`) is a config
error, and `register --replace` keeps the registration's `[network]` (and
mints through it).

**What steps see.** Host steps (`run:`, job hooks, composite steps, Node
actions' pre/main/post) get `HTTPS_PROXY` / `https_proxy` / `HTTP_PROXY` /
`http_proxy` and `NO_PROXY` / `no_proxy` (config mode: the proxy and the
bypass list including the loopback entries; environment mode: exactly what
the runner itself uses — a set-but-empty variable disables its pair, an
unparseable one falls back to `ALL_PROXY`, `REQUEST_METHOD` disables all of
them — re-exported in both casings), plus
`NODE_EXTRA_CA_CERTS=<ca_bundle>` when a bundle is set. Workflow, job,
`GITHUB_ENV` and step `env:` values win over these. Job containers (and the
`run:` steps executed in them), service containers and Docker actions get the
proxy variables unless their own `env` sets them — never
`NODE_EXTRA_CA_CERTS`, whose host path does not exist in a container.

**Secrets.** The proxy password (as written and percent-decoded) and its
`base64(user:password)` Basic credential — or a lone `token@` user name,
as written and decoded — are registered with the secret masker (in
environment mode, those of `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY`), so a step that prints `$HTTPS_PROXY` shows `http://***@…` and no log,
journal or diagnostic carries them. A value that does not parse cleanly
(for example an unencoded `/` in an environment proxy's password) is masked
whole. The masker ignores values shorter than
4 characters, so use a real password; runner-authored messages always print
the proxy redacted.

**Limits — configure these yourself.**

- **Docker daemon.** Image pulls and `docker build` for Dockerfile actions
  run inside `dockerd`, which uses its own proxy settings (systemd drop-in
  `HTTP_PROXY`/`HTTPS_PROXY`, or `proxies` in `~/.docker/config.json`) and
  its own CA trust (`/etc/docker/certs.d/`). The runner's Docker socket
  (`DOCKER_HOST`) never goes through the HTTP proxy.
- **Language tools.** Only Node reads `NODE_EXTRA_CA_CERTS`. git
  (`http.sslCAInfo` / `GIT_SSL_CAINFO`), curl and OpenSSL tools
  (`SSL_CERT_FILE`), Python requests (`REQUESTS_CA_BUNDLE`), pip and Java
  keystores need their own CA configuration; most tools also need the CA in
  the OS trust store. Node itself only uses `HTTPS_PROXY` when the action
  opts in (`@actions/http-client` does; plain `https.get` does not).
- Not supported: SOCKS proxies, separate http/https proxy URLs in config
  (use environment mode), PAC/WPAD, a `.proxy` file, or disabling TLS
  verification.

## Troubleshooting

| Symptom | Fix |
|---|---|
| Anything — start here | Run `toolu-runner doctor`. A `WARN` / `FAIL` row with a known fix is followed by the command that applies it. |
| `config not found at ...` / `no runner registration found under ...` | Run `register` before `run`. |
| `several runner registrations found (...)` | More than one repo is registered and the cwd matches none — `cd` into the repo, or pass `--config`. |
| `registration already exists at ...` | Pass `--replace` to `register`. |
| `... is not a git repository, so the repo cannot be inferred` / ``... has no `origin` remote`` at `register` | Zero-arg inference needs a cwd git repo with an `origin` remote — pass `--url` instead. |
| `no GitHub OAuth App configured` | The built-in device-flow App isn't wired yet — set `TOOLU_RUNNER_CLIENT_ID` (or `login --client-id`), or skip the device flow with `--token` / `TOOLU_RUNNER_TOKEN`. |
| `run` exits after one job (warned it would) | No stored `login` token, so re-mint has no bearer. Run `toolu-runner login` (or set `TOOLU_RUNNER_TOKEN`) to stay online, or re-register with a GitHub App (`register --app --replace`) so re-mint needs no personal token; or pass `--once` if a single job is intended. `doctor` flags this (and a token GitHub has revoked) as `FAIL  token`. |
| `re-mint rejected: GitHub App installation token failed with status 401 …` / `404 …` | GitHub no longer accepts the App: its private key was revoked, the App was deleted, or the installation was removed. Reinstall the App (the error names its install URL) or create a new one, then `toolu-runner register --app --replace`. |
| `the ephemeral registration was kept — if the runner was removed on GitHub …` (exit 1) | The runner was deleted on GitHub before it got a job (the token exchange answered `invalid_client`). Run `toolu-runner remove`, then `register` again. |
| `… (the job was acquired; its registration was removed)` (exit 1) | An ephemeral job was acquired but executing or reporting it failed; the registration was still cleaned up. Check the job on GitHub and `_diag/runner.log`. |
| `ephemeral runner … finished its job, but unregistering it on GitHub failed` (exit 2) | The job ran; fix the token or connectivity, then `toolu-runner remove` to finish the cleanup. |
| `… is an ephemeral registration … a service unit restarts run forever` | `install-service --config` does not supervise one-job registrations; use the machine service (`install-service` without `--config`), which does, or start `run` from a one-shot supervisor. |
| `another toolu-runner run --all (pid N, …) already supervises <home>` (exit 2) | The machine service (or another `run --all`) already runs every registration of this home; there is nothing to start. |
| `doctor`: `FAIL  run  stopped by the service at …: <error>` | `run --all` stopped that one registration (the others keep running). Fix the cause it names, then reload the service: the `fix:` line gives the command. |
| `doctor`: `FAIL  service  supervised by 2 units: …` | An older per-registration unit still runs next to the machine service; `toolu-runner install-service` replaces it. |
| `runner is already running as PID <pid> ...` | Another `run` holds this repo's `.lock`; its PID is in the error. Wait it out, or stop the holder with `kill -INT <pid>`. |
| `generate-jitconfig` fails with a network error | A firewall is blocking `api.github.com` (github.com) or the GHES host. Run `toolu-runner check` to see which hosts fail and why. |
| `run` hangs or fails before any job | Run `toolu-runner check`: it names the unreachable GitHub service, the failure class (`dns`/`tls`/`proxy`/…), and the proxy / CA settings in effect (including `[network]`). |

## Development

Requires Rust 1.94.1 (pinned in `rust-toolchain.toml`), `jq`,
[ast-grep](https://ast-grep.github.io), and cargo-nextest:

```sh
cargo install ast-grep --locked
cargo install cargo-nextest --locked
```

```sh
cargo build --workspace --all-targets   # also builds the test units, so the test run reuses the libraries
cargo nextest run --workspace    # default suite; ignored Docker/live lanes run separately
cargo test --workspace --doc     # doctests; nextest does not run them
cargo build --profile debugging  # opt-in full debuginfo for a debugger (target/debugging/)

./tools/check.sh all             # the full local gate
```

`./tools/check.sh all` is the one command CI mirrors layer-for-layer —
spread over parallel jobs, with the nextest run sharded (`--partition
slice:k/N`) across two ubuntu and three macOS runners, so a PR check
finishes in about half the serial time:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
the #[allow(..)] / #[expect(..)] ban (including crate-level forms; benches/ exempt)
bash scripts/guardrails/run.sh          # the structure gate
cargo nextest run --workspace
cargo test --workspace --doc            # nextest skips doctests
```

`scripts/guardrails/` is copied verbatim from
[toolu-conventions](https://github.com/Falconiere/toolu-conventions) and
is never hand-edited — change the crate's `guardrails.config.json`
instead. It enforces what the compiler cannot see: no `mod.rs` barrels,
no inline `#[cfg(test)] mod tests { … }` bodies, a 500 **code-line**
ceiling per file (tests exempt), `snake_case` filenames, a `README.md`
in every `src/` submodule folder, no `std::env::var` outside a crate's
`config` module, and no committed credential values. It exits 3 — never
0 — if `jq` or `ast-grep` is missing, so a broken setup can't look like
a clean run.

On macOS, builds and tests can stall on XProtect scans of fresh executables;
see [docs/macos-developer-tools.md](docs/macos-developer-tools.md).

`lefthook install` wires `cargo fmt` + the structure gate to
`pre-commit` and the full gate to `pre-push`.

The live suite talks to a real repo and is token-gated:

```sh
TOOLU_RUNNER_LIVE_TOKEN=<token> TOOLU_RUNNER_LIVE_REPO=owner/test-repo \
  cargo test -p toolu-runner --features live -- --ignored --test-threads=1
```

`TOOLU_RUNNER_LIVE_REPO` picks the target repo (the `live` workflow defaults to
`Falconiere/toolu-runner-test`).

### Manual deploy

`scripts/deploy.sh` builds every release artifact on an Apple Silicon Mac
and publishes the release without CI (`--help` lists the prerequisites):

```sh
scripts/deploy.sh --tag vX.Y.Z                # tag must match Cargo.toml; publishes release, images, Homebrew
scripts/deploy.sh --tag vX.Y.Z --change-log   # release notes from git-cliff, not CHANGELOG.md
scripts/deploy.sh --tag vX.Y.Z --dry-run      # print every command, run none
```

### Workspace

Twelve crates under `crates/` (ten libraries plus the `toolu-runner` and `toolu-daemon` bins), acyclic dependency graph:

- **`shared`** — config, errors, events, job-message types, tracing
  init, `SecretMasker`, platform labels. Sync, no network. No internal deps.
- **`protocol`** — JIT config, RSA/JWT, sessions, message decryption.
  Sync, I/O-free, **network-free** (no `reqwest`, no `tokio` — enforced
  by its `Cargo.toml`). → `shared`.
- **`network`** — the one place that builds the production `reqwest`
  clients (proxy and custom-CA support) and reads the proxy variables, so
  `protocol` stays free of `reqwest`. → `shared`.
- **`config`** — registration config (TOML/JSON), the per-repo
  `runners/<owner>/<repo>/` registry, cwd repo inference, the
  single-job file lock, and the login-token store. → `shared`.
- **`expressions`** — the `${{ }}` evaluator. → `shared`.
- **`cache`** — content-addressed CI cache (CAS + Twirp/blob/v1
  endpoints). → `shared`.
- **`wire`** — async HTTP transport (`net/`) + Run/Results reporting
  domain types (`reporting/`). → `shared`, `protocol`, `network`.
- **`observability`** — per-job JSONL journal (`_diag/jobs/`). →
  `shared`.
- **`execution`** — the job execution engine + Docker/Node + the
  `Runner`. → `shared`, `expressions`, `cache`, `network`.
- **`listener`** — the GitHub JIT lifecycle. → `execution`, `wire`,
  `observability`, `shared`, `protocol`.
- **`toolu-runner`** — the CLI **bin** (`register` / `run` / `boot` /
  `remove` / `status` / `install-service` / `login` / `logout` /
  `create-app` / `check` / `doctor` / `setup`). → `shared`, `protocol`, `network`,
  `config`, `wire`, `observability`, `listener`.
- **`daemon`** — the `toolu-daemon` bin: the HTTP service on a toolu-operated
  VPS host that turns `POST /v1/jobs` into one sysbox-isolated container
  running `toolu-runner boot` (see the [VPS host runbook](docs/vps-host-runbook.md)).
  No internal deps.

[docs/architecture.md](docs/architecture.md) has the full design with
sequence diagrams for register / run / cancel / reconnect.

## Contributing

PRs welcome. Before you open one:

1. `./tools/check.sh all` passes.
2. Listener or reporting change? Add coverage in the owning crate; use
   `crates/toolu-runner/tests/` for cross-crate CLI/lifecycle integration.
3. User-facing change? Update `README.md` and `docs/architecture.md` in the
   same commit. `CHANGELOG.md` is generated from conventional-commit subjects
   at release time (git-cliff), so don't edit it by hand.

New files stay under 500 **code** lines (blanks and comments don't
count; tests are exempt); function bodies under 150. Tests live in a
sibling `tests/` folder, never in the same file as the logic. Every new
`src/` submodule folder needs a `README.md` and an entry in that crate's
`guardrails.config.json` `src.requireReadme`. See `AGENTS.md` →
"Gate & house rules".

### Step failure and cancellation cleanup

A step execution error (including a missing shell/cwd or action manifest) is
reported under that step's ID and follows the same `continue-on-error` adjustment
as a nonzero exit. Its expression `outcome` remains `failure`; its `conclusion`
is `success` only when continuation applies. Condition and step-env evaluation
errors fail their step before continuation; job setup failures remain fatal.
Later conditions evaluate against the updated job state.

GitHub job cancellation re-evaluates the running condition. Eligible `always()`
work can finish, and later `always()`/`cancelled()` cleanup can run, within one
five-minute budget shared with action posts. Cleanup cannot change the final
cancelled result. SIGINT/SIGTERM runner shutdown interrupts running work, skips
later user steps/posts, tears down job resources, and reports failure.
Timeout and cancellation kill the owned Unix process group and reap its leader;
processes that deliberately detach from that group are terminated by the job-end
`RUNNER_TRACKING_ID` sweep instead (see *What runs*).
Reaping allows up to ten seconds and each output pipe up to two seconds after
termination. See [the evidence map](docs/test-coverage.md#issue-100--conditional-cleanup)
for verified cases and remaining live/reference lanes.


## License

MIT — see [LICENSE](LICENSE).

## Runner version and updates

Toolu reports the pinned GitHub compatibility baseline `2.337.0` consistently
for session, poll and acknowledgement; `toolu-runner --version` reports the
separate toolu product release. Updates are operator-managed. Refresh messages
warn and continue; operators must track GitHub's 30-day update requirement and
critical security updates. See [runner updates](docs/runner-updates.md) for
ownership, deployment, version-gating limits and revalidation.

### Step shells

On Linux and macOS, a `run:` step without a shell uses bash when executable on
its step `PATH`, otherwise sh, with `-e`. Linux job containers default to sh.
Explicit `shell: bash` uses `--noprofile --norc -e -o pipefail`; `shell: sh`
uses `-e`. Job/workflow `defaults.run.shell` supplies the shell for steps that
omit it; composite `run:` steps require their own `shell:`.

Use a custom template such as `shell: perl {0}` or `shell: python3 {0}` to run
another interpreter. `{0}` is the script path; quoted arguments and paths with
spaces stay intact. Templates are argument lists, without shell expansion.
Literal braces in arguments use `{{` and `}}`. Unknown bare shells, missing
`{0}`, malformed templates and unavailable executables fail the step explicitly.

The `python` built-in runs `python` with a `.py` script, so that executable must
be installed. `pwsh` runs a `.ps1` script with terminating PowerShell errors and
propagation of the last native-command exit code. These rules also apply to
composite steps and container execution. Windows shells are outside the current
support scope. See [measured shell coverage](docs/test-coverage.md#shell-templates-80)
for verified lanes and remaining reference/backend evidence.

### Problem matchers

`::add-matcher::tsc.json` registers GitHub Actions problem matchers for the
current job. Registrations persist into later steps and nested actions, as verified
by the [captured-message replay tests](crates/execution/src/execution/tests/problem_matcher_replay.rs).
`::remove-matcher owner=tsc::` removes an owner case-insensitively; the file
form `::remove-matcher::tsc.json` removes all owners listed in that document.
Adding an existing owner replaces it atomically. Shell, Node and composite
stdout/stderr are matched, preserving their original log text. Annotations use
the existing masked per-step reporting path; an error-severity match alone does
not change the process conclusion. Invalid matcher commands fail the step.

Matchers support capture fields, default severity, `fromPath` and multiline
final-pattern `loop`. Relative matcher-file paths use the job workspace.
Relative diagnostic paths use the parent of `fromPath` and then the workspace,
independently of the process working directory. File links require an existing
file under a checkout whose `.git/config` identifies the workflow repository.
Partial multiline state is isolated per process stream; registrations are job-wide.

The regex engine supports Rust `regex` syntax, not unrestricted .NET regex:
lookaround and backreferences are rejected explicitly. Limits are 1 MiB per JSON
file, 64 owners, 32 patterns per owner, 16 KiB regex source and 256 KiB compiled
regex per pattern. Lines over 64 KiB remain in logs but skip matching and reset
partial state, with one warning per process. These bounds protect execution from
pathological patterns. Live GitHub/GHES UI and pinned reference-runner parity
remain **unverified**; see [test coverage](docs/test-coverage.md).

### Set up job diagnostics

The **Set up job** log reports the toolu version and GitHub compatibility version,
actual host OS/architecture/machine name, JIT runner name, and job-supplied runner
group. Missing identity metadata is shown as `unavailable`. When supplied by
GitHub, token permission scopes appear in a collapsible group and the secret
source is listed; token values are never intentionally printed.

Successful remote action preparation lists the intended `owner/repo[/path]@ref`
and resolved immutable SHA, including cached actions. Repeated identical ref/SHA
pairs are listed once per job. Background and nested action preparation can append
these diagnostics after the first workflow step starts. Setup completes at that
first step; early setup failure or cancellation keeps its diagnostics and reports
the corresponding conclusion. Setup output uses the same masking and live/durable
log paths as step output. See [setup evidence](docs/test-coverage.md#set-up-job-diagnostics-87)
for tested cases and unverified backend lanes.

### Completion payload and deployment links

The last row of every job is **Complete job**, as on GitHub-hosted runners.
It evaluates the job's `outputs:` and then its `environment.url` after every
step and post has run, so a URL built from `steps.<id>.outputs` gives the
deployment its "View deployment" link. The URL may use the `github`,
`needs`, `strategy`, `matrix`, `steps`, `job`, `runner`, `env` and `vars`
contexts, but not `secrets`. If the value contains a masked secret, the URL
is dropped and the step warns
`Skip setting environment url as environment '<name>' may contain secret.`.
toolu also drops a literal URL that contains a secret, which the official
runner would send.

Each step result reported to GitHub carries upstream's action identity:
`run` plus the shell, `node20`/`node24`, `composite`, `Dockerfile`, or
`DockerHub` plus the image, along with the action's name and ref. A failed
action download or resolution caused by GitHub's service sets
`infrastructureFailureCategory` (`resolve_action`, `error_download_action`,
`invalid_action_download`). A missing repository, ref or permission does not
set it. See `docs/test-coverage.md` (#88) for what is verified live.
