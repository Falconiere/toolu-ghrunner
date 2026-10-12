# `toolu-runner check` diagnostics subcommand — Design

**Date:** 2026-10-12   **Status:** Approved   **Author:** Claude (epic #67 worker)   **Topic:** issue #93 — an operator preflight equivalent to upstream `run.sh --check`

## Problem

Operators have no built-in way to answer "can this host run GitHub Actions
jobs?" before (or after) registering. When a firewall, proxy or TLS
interception blocks one GitHub service, the first symptom is a stuck or
failing `run`, with the cause buried in `_diag/runner.log`. Upstream ships
`run.sh --check` (`src/Runner.Listener/Checks`, actions/runner `cab9d1c`:
`InternetCheck`, `ActionsCheck`, `GitCheck`, `NodeJsCheck`). toolu has no
equivalent (`crates/toolu-runner/src/cli.rs`, no `check` command).

## Non-Goals

1. Proxy / custom-CA **configuration** (config keys, `.proxy` file, exporting
   `HTTPS_PROXY`/`NODE_EXTRA_CA_CERTS` to steps) — owned by #94. `check`
   reports what is effective today.
2. Validating credentials: no PAT, login token or JIT credential is sent or
   checked; probes are unauthenticated.
3. Machine-readable output (`--json`) and writing a `_diag` check log.
4. Upstream's POST probe to the pipelines service and its `git ls-remote` /
   Node HTTPS round trips — a diagnostic stays GET-only and side-effect free.
5. Auto-downloading an extra CA (upstream `DownloadExtraCA`).
6. Windows.

## Architecture

A new `check` subcommand in the bin crate. It never acquires the per-repo
`.lock`, never creates files, never opens a session and never registers. It
runs three groups concurrently, each probe individually bounded, then prints
one report and returns an explicit exit code (the `boot` pattern — `main`
applies the code directly instead of the generic Ok→0 / Err→2 mapping).

**1. Endpoint selection (pure).** Hosts come from, in order:
`--url <github url>` (no registration read) > `--config <path>` (must load,
else exit 2) > the registration `run` would resolve (`resolve_config(None)`;
when it fails — none or ambiguous — fall back to github.com defaults and say
why) > github.com. With a registration, the JIT config's `.runner`
`ServerUrl` (pipelines), `ServerUrlV2` (broker) and `.credentials`
`AuthorizationUrl` (token service) are probed exactly as configured; the
other hosts derive from `runner_url`. Host derivation mirrors upstream
`ActionsCheck.cs`:

| Probe | Required | github.com (`host == github.com`) | GHES (any other host `H`) |
| --- | --- | --- | --- |
| `api` | yes | `https://api.github.com/` | `<scheme>://H/api/v3` |
| `codeload` | yes | `https://codeload.github.com/_ping` | `<scheme>://codeload.H/_ping`; **skip** when `H` is an IP literal (no subdomain can exist; archives then come from `H`, covered by `api`) |
| `pipelines` | yes | registration `ServerUrl`, else `https://pipelines.actions.githubusercontent.com/_apis/health` | registration `ServerUrl`, else `<scheme>://H/_services/pipelines/_apis/health` |
| `broker` | yes | registration `ServerUrlV2`, else `https://broker.actions.githubusercontent.com/health` | registration `ServerUrlV2` when non-empty and different from `ServerUrl`, else **skip** (GHES V1 has no broker) |
| `token` | yes | registration `AuthorizationUrl`, else `https://vstoken.actions.githubusercontent.com/_apis/health` | registration `AuthorizationUrl`, else `<scheme>://H/_services/vstoken/_apis/health` |
| `results` | yes | `https://results-receiver.actions.githubusercontent.com/health` | **skip** (no Results service on GHES, as upstream) |
| `node-dist` | no | `https://nodejs.org/dist/` (where the runner downloads Node runtimes) | same |

`<scheme>` is the `--url`/`runner_url` scheme, so `http://` GHES works.

**2. HTTP probes.** One `reqwest::Client` built like production
(`Client::builder()` — default system proxy + `rustls-tls-native-roots`) plus
`timeout`/`connect_timeout` = `--timeout` and redirects disabled. Each probe is
a GET. **Pass = any HTTP response from the target** (status printed), because
the configured broker/tenant/token URLs legitimately answer 401/404 to an
unauthenticated GET, except HTTP 407 (proxy authentication required), which
fails. Errors are classified from the `reqwest::Error` chain into `timeout`,
`dns`, `tls`, `connect` (refused/reset/unreachable), `proxy` and `other`,
each with a one-line hint. Probes run concurrently with `futures`-free
`tokio::task::JoinSet`; total wall time ≈ one timeout.

**3. Tools (optional — never fail the run).** Each a real subprocess via
`tokio::process` with `kill_on_drop` and the same timeout:
- `git --version`.
- `docker version --format '{{.Client.Version}}|{{.Server.Version}}'` plus the
  daemon endpoint the runner itself uses (`DOCKER_HOST`, else
  `unix:///var/run/docker.sock`); "CLI present, daemon unreachable" shows the
  first stderr line. On non-Linux, notes that container jobs/actions are
  Linux-only.
- `node --version` (optional — the runner auto-downloads Node) plus the
  cached runtimes under `<data_dir>/node/` when a registration resolved.
A missing binary prints `not found on PATH` and the capability it limits.

**4. Environment report.** Reads only through `crates/toolu-runner/src/config.rs`
(the `no-direct-env-var` guardrail):
- Proxy: `HTTPS_PROXY`/`https_proxy`, `HTTP_PROXY`/`http_proxy`,
  `ALL_PROXY`/`all_proxy`, `NO_PROXY`/`no_proxy`, first-set wins in that
  upper→lower order (hyper-util `Matcher::from_system`, which reqwest 0.12.28
  uses regardless of its `system-proxy` feature). `REQUEST_METHOD` set (CGI)
  is reported because hyper-util then ignores **every** proxy variable.
- TLS trust: `SSL_CERT_FILE`, `SSL_CERT_DIR` (when either is set
  rustls-native-certs 0.8 loads **only** those, replacing the platform store),
  the count of trust anchors and load errors from
  `rustls_native_certs::load_native_certs()` (the same call reqwest makes),
  and `NODE_EXTRA_CA_CERTS` (seen by Node steps, which inherit the runner env).
  reqwest is built with `rustls-tls-webpki-roots` as well (unified on by
  `opendal`, declared explicitly in the bin), so the compiled-in Mozilla
  roots are always trusted on top; the report says so.
- Redaction: a proxy URL prints as `scheme://***@host:port` when it carries
  userinfo, with path/query dropped; an unparseable value prints
  `<set, unparseable — value hidden>`. Every printed line also passes through
  `shared::SecretMasker` seeded with each proxy's raw userinfo, username and
  password (and the same parts of a `DOCKER_HOST` URL), so a credential echoed
  inside a library or tool error is masked too.

Reuse: `crate::resolve_config`, `config::config::load_config`,
`protocol::JitConfig::parse`, `shared::SecretMasker`, the
`boot_cmd` explicit-exit-code path in `main.rs`. New direct dependency:
`rustls-native-certs = "0.8"` (already in `Cargo.lock` at 0.8.4 via reqwest).

## Interfaces / Schema

```text
toolu-runner check [--url <URL>] [--config <PATH>] [--timeout <SECS>]
  --url      GitHub URL whose hosts to probe (github.com, GHES, or a repo/org
             URL on either). Overrides any registration. Conflicts with --config.
  --config   Registration whose JIT endpoints to probe (default: resolved like
             `run`; none found → github.com defaults).
  --timeout  Per-probe bound in seconds, 1..=300, default 10.
```

Files (flat in `crates/toolu-runner/src/`; the crate allows no nested module
folders besides `tests/`):

| File | Role |
| --- | --- |
| `check_cmd.rs` | `cmd_check(CheckArgs) -> i32`: target resolution, concurrent run, exit code |
| `check_endpoints.rs` | pure `Target`, `Endpoint { name, url: Option<String>, required, skip_reason }`, `endpoints_for(target)` |
| `check_probe.rs` | `probe_http(client, &Endpoint) -> ProbeOutcome`, `classify(&reqwest::Error) -> FailureKind` |
| `check_tools.rs` | `probe_git/probe_docker/probe_node(timeout) -> ToolReport` |
| `check_env.rs` | pure `redact_proxy(&str) -> String`, `ProxyReport`, `TrustReport` |
| `check_report.rs` | `render(&Report, &SecretMasker) -> String`, `exit_code(&Report) -> i32` |

Report lines (stable prefixes, one per check):

```text
toolu-runner check — target github.com (registration: <path> | --url | defaults: <why>)
network (required):
  PASS  api        https://api.github.com/  HTTP 200  41 ms
  FAIL  broker     https://…/  dns: <error>  hint: <hint>
  SKIP  results    not applicable on GHES
network (optional):
  WARN  node-dist  https://nodejs.org/dist/  timeout after 10 s  hint: …
tools (optional):
  OK    git        git version 2.43.0
  WARN  docker     CLI 29.1.3; daemon unreachable: <first stderr line>  endpoint: unix:///var/run/docker.sock
  WARN  node       not found on PATH (the runner downloads Node for actions)  cached: none
proxy:
  HTTPS_PROXY  http://***@proxy.internal:3128
  NO_PROXY     (unset)
tls trust:
  SSL_CERT_FILE  /etc/custom/ca.pem (replaces the platform store)
  anchors        1 loaded, 0 errors
result: 1 required check failed (exit 1)
```

Exit codes: `0` every applicable required probe passed (optional WARNs
allowed); `1` at least one required probe failed; `2` usage/config error
before probing (bad `--url`, explicit `--config` that cannot load, invalid
`--timeout`; clap usage errors keep clap's own code 2).

## Failure modes and edge cases

- **Hanging endpoint** (accepts TCP, never answers): bounded by `--timeout`;
  reported `timeout`; aggregate exit 1 if required.
- **DNS failure** (`.invalid`), **connection refused**, **TLS failure**
  (untrusted CA / wrong host): classified FAIL with hint; never retried.
- **Proxy**: CONNECT refused/closed/407 → FAIL (`proxy` class when reqwest
  attributes it to the proxy, else `connect`, and the hint names the
  configured proxy, redacted); credentials never printed.
- **HTTP 401/403/404 from a target:** PASS with the status shown — the host is
  reachable; credentials are deliberately not checked (Non-Goal 2). HTTP 407
  (from a proxy on a plain-`http` target) is the one status that FAILs.
- **`DOCKER_HOST` with userinfo** (`tcp://user:pass@host`): printed through
  the same URL redaction as proxies.
- **No registration / ambiguous registrations:** not an error — github.com
  defaults with the reason on the header line.
- **Explicit `--config` missing/unparseable, or its JIT config fails to
  parse:** exit 2 with the error (credential blobs are never printed).
- **`--url` invalid / non-http(s) scheme:** exit 2.
- **Tool missing / hangs / non-zero exit:** WARN with the reason; never
  changes the exit code; hung subprocess killed at the timeout.
- **`SSL_CERT_FILE` points at a missing or empty file:** rustls-native-certs'
  error and "0 loaded — only the bundled roots remain; a private CA will
  fail"; public hosts still verify against the bundled Mozilla set.
- **Concurrent with a running `run`:** safe — no lock, no writes.

## Acceptance criteria

- **AC-1:** Given `--url http://127.0.0.1:<port>` pointing at a real local
  HTTP server, `check` GETs `/api/v3`, `/_services/pipelines/_apis/health` and
  `/_services/vstoken/_apis/health`, prints `PASS` for `api`, `pipelines`,
  `token`, `SKIP` for `codeload` (IP literal), `broker` and `results` (GHES),
  and exits 0.
- **AC-2:** Given `--config` naming a registration whose JIT config's
  `ServerUrl`/`ServerUrlV2`/`AuthorizationUrl` point at distinct paths on a
  local server, the server receives GETs for exactly those configured paths
  and the report names those URLs.
- **AC-3:** Given a required endpoint that accepts TCP and never responds and
  `--timeout 1`, `check` reports `timeout` for it, finishes in well under 10 s,
  and exits 1.
- **AC-4:** Given `--url https://toolu-check.invalid` (DNS failure), a
  closed local port (connection refused), or `--url https://127.0.0.1:<port>`
  where the port serves plaintext HTTP (TLS handshake failure), the required
  probes report `FAIL` with the `dns` / `connect` / `tls` class and `check`
  exits 1.
- **AC-5:** Given `HTTP_PROXY=http://user:<secret>@127.0.0.1:<proxy port>` and
  an `http://` target, the local proxy receives the probe requests in
  absolute-form with a `Proxy-Authorization` header; the report shows
  `http://***@127.0.0.1:<proxy port>` and the secret appears nowhere in
  stdout or stderr. With `NO_PROXY=127.0.0.1` the target server receives the
  requests directly and the proxy receives none. Given a proxy that answers
  `407 Proxy Authentication Required`, the probes report `FAIL` with the
  `proxy` class and `check` exits 1. With `REQUEST_METHOD` set, the proxy
  section states that every proxy variable is ignored (CGI guard) and the
  target is reached directly.
- **AC-6:** Given `SSL_CERT_FILE` set to a committed real PEM CA certificate,
  the report shows that path, notes it replaces the platform store and shows
  `1 loaded`; given a missing path it shows the load error and `0 loaded`.
  `NODE_EXTRA_CA_CERTS` is echoed when set.
- **AC-7:** On a host with git / docker / node on `PATH`, each tool line
  contains the exact version the real binary prints (`git --version`,
  `node --version`, docker client version); with `PATH` restricted to an
  empty directory each tool reports `not found on PATH` and the exit code is
  unaffected (still 0 against a healthy target). A `git` on `PATH` that never
  exits (a shell script running `sleep 30`) is reported `WARN … timed out`
  and `check` still finishes within the bound. With the real docker CLI and
  `DOCKER_HOST=unix:///<tmp>/absent.sock`, the docker line is `WARN` with
  `daemon unreachable` and shows that endpoint (Linux CI, where the docker CLI
  exists; on a host without the CLI the case is recorded as unverified).
  With `DOCKER_HOST=tcp://user:<secret>@127.0.0.1:<closed port>` the endpoint
  prints as `tcp://***@127.0.0.1:<port>` and `<secret>` appears nowhere in
  stdout/stderr (also when the docker CLI echoes the host in its error — the
  masker is seeded with the `DOCKER_HOST` userinfo too).
- **AC-8:** `check` creates nothing under `TOOLU_RUNNER_HOME` (an empty home
  stays empty, a registration dir gains no `.lock`/files), runs normally while
  another process holds that registration's `.lock` (fs2 exclusive lock, as a
  live `run` would), and the target server only ever receives `GET` requests.
- **AC-9:** Exit code 2 for `--url ftp://x`, `--timeout 0`, `--config`
  naming a missing file, and `--config` whose `jit_config` is not a valid JIT
  envelope (its value never printed); `--url` with `--config` is a clap usage
  error.
- **AC-13:** Given an empty `TOOLU_RUNNER_HOME`, a cwd outside any git repo
  and no flags, the header reads `defaults:` with the reason and the probes
  target the github.com hosts: with `HTTPS_PROXY` pointing at a local
  recording proxy that closes every connection, the proxy records `CONNECT`
  for `api.github.com:443`, `codeload.github.com:443`,
  `pipelines.actions.githubusercontent.com:443`,
  `broker.actions.githubusercontent.com:443`,
  `vstoken.actions.githubusercontent.com:443` and
  `results-receiver.actions.githubusercontent.com:443`, the required probes
  FAIL (proxy refused the tunnel), and `check` exits 1 — hermetic, no traffic
  leaves the host.
- **AC-12:** Given a local target whose token path answers `401`, the `token`
  line is `PASS … HTTP 401` and the exit code stays 0.
- **AC-10:** Live: `check` against real github.com from a networked host
  passes `api`, `codeload`, `pipelines`, `broker`, `token`, `results` with the
  real HTTP statuses; re-run with `SSL_CERT_FILE` set to the single committed
  ISRG root, the report shows `1 loaded` plus the always-trusted bundled
  Mozilla set, and the github.com probes still PASS — the live run proved the
  compiled-in webpki roots cannot be removed (see Deviations in the plan).
- **AC-11:** `./tools/check.sh all` is green.

## Acceptance evidence

| AC | Real input | Expected observable | Check |
| --- | --- | --- | --- |
| AC-1 | axum server on 127.0.0.1 recording method+path | lines + exit 0 + recorded paths | `cargo test -p toolu-runner --test check_cmd_test healthy_ghes_target` |
| AC-2 | JIT envelope built from `tests/fixtures/jit_config_ghes.json` shape with URLs rewritten to the local server, written to a temp registration | recorded paths == configured paths | `… check_cmd_test configured_registration_endpoints` |
| AC-3 | raw `TcpListener` that accepts and holds sockets | `timeout`, elapsed < 10 s, exit 1 | `… check_cmd_test hanging_endpoint_is_bounded` |
| AC-4 | `.invalid` TLD (RFC 6761), bound-then-dropped port, `https://` to the plaintext axum server | `dns` / `connect` / `tls`, exit 1 | `… check_cmd_test dns_refused_and_tls_fail` |
| AC-5 | recording proxy server (absolute-form + header capture); a 407-answering proxy | proxy hits / direct hits, redaction, no secret in output; `proxy` FAIL exit 1 | `… check_cmd_test authenticated_proxy_redacted`, `… no_proxy_bypasses`, `… proxy_407_fails` |
| AC-6 | `tests/fixtures/check/isrg-root-x1.pem` (public Let's Encrypt root) | path + `1 loaded`; missing → `0 loaded` + error | `… check_cmd_test ssl_cert_file_reported` |
| AC-7 | the host's real git/docker/node; empty-dir PATH; a `git` script that sleeps | exact versions from running the same binaries in the test; `not found on PATH`, exit 0; `timed out`, bounded | `… check_cmd_test tool_versions_match_real_binaries`, `… missing_tools_are_optional`, `… hung_tool_is_bounded` |
| AC-7b | `DOCKER_HOST` with userinfo, closed port | redacted endpoint; secret absent | `… check_cmd_test docker_host_credentials_redacted` |
| AC-13 | empty home + non-repo cwd + recording CONNECT proxy | `defaults:` header; recorded CONNECT authorities == the six github.com hosts; exit 1 | `… check_cmd_test defaults_target_github_com_hosts` |
| AC-8 | empty temp home; temp registration dir | dir listing unchanged; only GET recorded | asserted in `healthy_ghes_target` / `configured_registration_endpoints` |
| AC-9 | bad args; registration with a garbage `jit_config` | exit 2 / clap usage; garbage value absent from output | `… check_cmd_test usage_errors_exit_2` |
| AC-12 | axum route answering 401 on the token path | `PASS … HTTP 401`, exit 0 | `… check_cmd_test unauthenticated_401_is_reachable` |
| AC-10 | real github.com; ISRG-only `SSL_CERT_FILE` | PASS lines with statuses; narrowed bundle still PASSes and the report names the bundled roots | `cargo test -p toolu-runner --features live --test check_cmd_test -- --ignored live_github_com` (run manually, output recorded in the PR; network-dependent so outside the hermetic gate) |
| AC-11 | repo | green | `./tools/check.sh all` |

Pure unit coverage (bin crate, `src/tests/`): endpoint derivation for
github.com / GHES / IP literal / registration overrides, proxy and
`DOCKER_HOST` redaction (userinfo, no userinfo, unparseable, path/query),
exit-code aggregation.

Platform applicability: Linux and macOS (CI runs both); Docker daemon line
is Linux-meaningful, macOS reports the CLI/daemon as found. GitHub.com is
verified live (AC-10); GHES derivation is verified against a local server
only — **GHES live is unverified** (no GHES instance available).

## Documentation impact

- `README.md` — CLI usage gets `check` with the exit-code table.
- `docs/architecture.md` — command list entry if it enumerates subcommands.
- `docs/test-coverage.md` — #93 row mapping AC/scenario → test.
- `AGENTS.md` — `toolu-runner/` module list gains the `check_*.rs` files and
  the `check` exit-code rule.
- `cli.rs` top-level Examples footer gains `toolu-runner check`.

## Open Questions

1. GHES codeload subdomain: following upstream (`codeload.H`) can falsely fail
   on a GHES with subdomain isolation off. Decision: keep upstream parity for
   DNS-name hosts; the hint names subdomain isolation. Non-blocking.
2. Whether `node-dist` should be required: decided optional — a cached runtime
   makes it unnecessary and the issue's required host list omits it.
   Non-blocking.

## Spec review (2026-10-12)

Round 1 — Needs changes (Jev coverage scores S1 2.44 / S2 1.97 / S3 1.82; unproven failure modes 0.76):
- Acceptance criteria: 🔴 blocker: TLS failure, 401-is-reachable, proxy 407 and bad-JIT `--config` had no AC. Added to AC-4, AC-5, AC-9, new AC-12.
- Acceptance criteria: 🟡 should-fix: custom CA effectiveness unproven. AC-10 live re-run with a narrowed `SSL_CERT_FILE` must fail `tls`.
- Failure modes: 🟡 should-fix: defaults fallback, hung tool, held `.lock`, CGI guard, docker daemon unreachable and `DOCKER_HOST` redaction unproven. Added AC-13, extended AC-5/AC-7/AC-8, AC-7b.

Round 2 — Approved: S1 2.51 / S2 2.46 after fixes; the least-proven mode (`DOCKER_HOST` redaction, Jev choice 0.71) now has AC-7b. GHES live and macOS docker-daemon cases are explicitly recorded as unverified, not passing. Open questions are decided and non-blocking.
