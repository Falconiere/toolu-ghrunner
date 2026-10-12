# Explicit proxy and custom CA configuration — Design

**Date:** 2026-10-12   **Status:** Approved   **Author:** Claude (epic #67 worker)   **Topic:** issue #94 — an optional `[network]` config applied to every runner HTTP client and exported to steps

## Problem

Proxy and CA support is implicit. Every runner HTTP client is a bare
`reqwest::Client::builder()` (10 sites) or `Client::new()` (1), so it reads
`HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY`/`NO_PROXY` through hyper-util's
`Matcher::from_system` and trusts OS roots plus the compiled-in Mozilla set.
The live-log WebSocket (`tokio_tungstenite::connect_async`) ignores proxies
entirely, and the S3 cold tier uses opendal's own client. There is no config
key, no documentation beyond `check`'s report, and job/service/action
containers receive no proxy settings. A runner behind a corporate proxy with a
TLS-intercepting CA has to rely on process env and on every tool agreeing.

Upstream (`actions/runner` `cab9d1c`, `RunnerWebProxy.cs`, `HostContext.cs`,
`ContainerInfo.cs`): proxy is env-only (`http_proxy`/`https_proxy`/`no_proxy`,
lower case first); the runner re-exports both casings into its own process env
so host steps inherit them; job, service and container-action containers get
`HTTP(S)_PROXY`/`NO_PROXY` in both casings unless the container's `env` sets
them (`TryAdd`); loopback destinations always bypass the proxy; the proxy
password is registered with the secret masker. There is no CA setting:
`docs/checks/sslcert.md` tells operators to trust the CA in the OS store and
configure git/node separately, and never to skip TLS validation.

## Non-Goals

1. A `.proxy` / `.certificates` file, or CLI flags for proxy/CA. `config.toml`
   is the single persisted config.
2. A TLS-verification bypass (`danger_accept_invalid_certs`, an equivalent of
   upstream's `GITHUB_ACTIONS_RUNNER_TLS_NO_VERIFY`). Never added.
3. Narrowing trust: `ca_bundle` **adds** anchors; OS roots stay, and in the
   shipped binary the compiled-in Mozilla set stays (comemory 99bd07ca).
4. Configuring the Docker daemon or its socket: image pulls and `docker build`
   of Dockerfile actions run inside `dockerd` (its own proxy config:
   systemd drop-in / `~/.docker/config.json` `proxies`), and the runner's
   bollard connection (`Docker::connect_with_host`, `DOCKER_HOST`) never uses an
   HTTP proxy or `ca_bundle`. Documented, not changed.
5. Configuring language tools' CA trust (git `http.sslCAInfo`, pip, curl/OpenSSL
   `SSL_CERT_FILE`, Java truststores, `REQUESTS_CA_BUNDLE`). Only
   `NODE_EXTRA_CA_CERTS` is exported, to host steps; the rest is documented.
6. `[network]` for `boot` (no config file — env only), `login`, `create-app`
   and the first `register` (no registration config exists yet — env only).
   `remove`, `run`, the `run` loop's re-mint, ephemeral unregister and `check`
   use the registration's `[network]`.
7. SOCKS proxies (reqwest built without `socks`), split http/https proxy URLs
   in config (env mode still supports them), PAC/WPAD.
8. The `daemon` crate (a separate server; reqwest only in its tests).
9. Wiring the S3 cold tier into production (`build_cas_handles` never calls
   `CasStore::with_l2` today); only its client is routed.
10. Windows.

## Architecture

**A new `network` crate owns client construction.** `protocol` depends on
`shared` and must stay free of `reqwest` (CI greps `cargo tree -p protocol`),
so the builder cannot live in `shared`. New crate `crates/network`
(deps: `shared`, `reqwest` with `rustls-tls-native-roots`, `rustls` (trust
anchor validation), `base64`, `percent-encoding`) sits below `wire`, `cache`, `execution`,
`listener` and the bin, all of which gain it as a dependency:

- `network::client_builder(&NetworkConfig) -> Result<reqwest::ClientBuilder, RunnerError>`
  — the only production `reqwest` client construction; each site adds only its
  own timeouts/redirect policy.
- `network::step_env(&NetworkConfig) -> StepNetworkEnv` — the values exported
  to steps (env-mode reads go through `network/src/config.rs`, the crate's env
  accessor module).
- `network::proxy_secrets(&NetworkConfig) -> Vec<String>`.

`shared` keeps only pure data and string helpers: `NetworkConfig` (part of
`RunnerConfig`, so `execution` sees it) with a redacting `Debug`, and
`shared::network_redact::redact_proxy_url`. Layering after the change:
`network` — `shared`; `wire` — `shared`, `protocol`, `network`; `cache` —
`shared`, `network`; `execution` — `shared`, `expressions`, `cache`, `network`;
`listener` adds `network`; bin adds `network`.

**Two modes, decided by `[network] proxy`.**

| | env mode (`proxy` unset — today) | config mode (`proxy` set) |
| --- | --- | --- |
| Runner clients' proxy | reqwest system proxy, unchanged (`Matcher::from_system`: upper case before lower, `ALL_PROXY` fallback, `REQUEST_METHOD` disables) | `Proxy::all(proxy).no_proxy(NoProxy::from_string(bypass))`; `ClientBuilder::proxy` disables the system proxy (reqwest 0.12.28 `client.rs:1416`), so env proxy vars **and env `NO_PROXY`** are ignored |
| Bypass list | env `NO_PROXY`/`no_proxy` | `no_proxy` entries **plus** `localhost`, `127.0.0.1`, `::1` (upstream's implicit loopback bypass; keeps offline/accelerated local service URLs direct for steps too) |
| Trust | OS roots ∪ Mozilla roots (bin) ∪ `ca_bundle` — `ca_bundle` is independent of `proxy` | same |

`ca_bundle` rules (checked on every `client_builder` call, fail closed with
`RunnerError::Config` naming the path): must be absolute after the config
crate's `~` expansion (`shared::paths::expand_tilde`) — a relative path is
rejected because a supervised service's cwd is not the operator's; must be a
readable regular file; parsed with rustls-pki-types'
`CertificateDer::pem_slice_iter`: a malformed PEM item fails the whole
bundle, non-certificate PEM items (keys) are ignored, zero certificates fails,
and every certificate must be accepted by rustls' `RootCertStore::add` as a
trust anchor. `proxy` must parse as an absolute `http://` or `https://` URL
with a host, and an `@` outside the parsed credentials (an unencoded reserved
character in the user name or password) is rejected; anything else is a
`RunnerError::Config` showing the URL redacted. `proxy = ""` and
`ca_bundle = ""` are unset. `describe` runs the same validation, so `check`
exits 2 on an unusable `[network]` before probing.

**Every HTTP client** (inventory, with the change):

| Site | Today | After |
| --- | --- | --- |
| `run` loop client (`network_setup::lifecycle_client`, replacing `run_cmd::build_loop_client`) — listener, re-mint, ephemeral unregister | built once at startup | built from the reloaded `[network]` **every iteration** (the loop's existing "edits take effect next pass" contract); a build error is fatal (exit 2) exactly like a corrupt config today — under systemd/launchd the supervisor restarts it and it fails again visibly, never silently dropping the CA |
| `remove` unregister (`main.rs`) | bare | registration `[network]` |
| `check` probes (`check_probe::build_client`) | bare | registration `[network]` when one resolves (`--config` or inferred); `--url` / no registration → env mode |
| job client (`execution` — action API, node runtime, post-drain) | bare in `job_runner.rs` | `RunnerConfig.network`, moved with the other job clients to a new `job_runner/job_http.rs` (`job_runner.rs` is at 491/500 code lines) |
| action archive client (`actions::downloader::request_archive`, private no-redirect client; `fetch_tarball_reader`'s client param is unused) | bare, per download | `ActionFetcher` owns one no-redirect archive client built from the job's network; `download_and_extract_action(archive_client, …)` uses it |
| OIDC upstream client (`oidc::server`, process-global `OnceLock`; `OidcServer` has no production caller) | bare, global | held in `OidcState`, built from `OidcConfig.network` (default env mode) |
| accelerated cache upstream (`job_runner.rs:693` `Client::new()`) | bare | `client_builder(network)` (no timeout, as today), in `job_runner/job_http.rs` |
| S3 cold tier (`cache::tier::l2::L2Tier::from_config`) | opendal's own client | `from_config(cfg, client)`; `S3::http_client(HttpClient::with(client))` |
| live-log WebSocket (`wire::reporting::live_log`) | `connect_async`, no proxy, native roots | HTTP/1.1 `Upgrade: websocket` GET on a caller-supplied client (`listener` builds it from `runner_cfg.network` with `http1_only()`), `wss→https`/`ws→http`, 101 + `Sec-WebSocket-Accept` verified, upgraded stream wrapped with `WebSocketStream::from_raw_socket(…, Role::Client, None)`; `WsStream` becomes `WebSocketStream<reqwest::Upgraded>`; the 30 s `try_connect` bound is kept; `tokio-tungstenite`'s TLS feature is dropped. Failure stays non-fatal (WARN, job runs without live log) |
| `boot`, `login`, `create-app`, first `register` | bare | `client_builder(&NetworkConfig::default())` (env mode, unchanged behaviour, one builder) |
| `docker::client::DockerClient::create_container` | — | not an HTTP client; no production caller; unchanged |

A source-inventory test pins that no production file outside
`crates/network/src` calls `reqwest::Client::builder()` or
`reqwest::Client::new()` (clippy `disallowed-methods` would also hit ~150 test
call sites, and `#[allow]` is banned).

**Export to steps.** `network::step_env` computes the effective values once
per job:

- config mode: `proxy` → `HTTPS_PROXY`, `https_proxy`, `HTTP_PROXY`,
  `http_proxy`; bypass list (with the loopback additions) joined by `,` →
  `NO_PROXY`, `no_proxy`.
- env mode: the runner env's first-set `HTTPS_PROXY`/`https_proxy`,
  `HTTP_PROXY`/`http_proxy`, `NO_PROXY`/`no_proxy` (upper case first — the
  runner's own reading order), each re-exported in **both** casings (upstream
  parity: curl/wget honour lower case, Docker only upper). `ALL_PROXY` is not
  translated (host steps still inherit it).
- `ca_bundle` set → `NODE_EXTRA_CA_CERTS=<absolute path>` (host only).

Host precedence (lowest → highest): runner process env (minus
`TOOLU_RUNNER_*`) < **network export** < workflow/job env < `GITHUB_ENV` <
step `env:`. Two fold points carry it, both host-only:

1. `ExecutionContext::inherited_process_env()` = `safe_process_env_vars()`
   overlaid by the context's network host env; it replaces the direct
   `safe_process_env_vars` folds in `steps_runner/script_support.rs`,
   `composite_env.rs` and `job_hooks.rs` (all fold with `or_insert` after the
   workflow env). `script_support.rs` and `composite_env.rs` skip the fold when
   a job container is set; `job_hooks.rs` folds unconditionally because hooks
   always run on the host, so hooks keep the overlay in container jobs too.
2. Node actions build a **delta** env and inherit the process env in
   `node_exec::build_node_command`; `node_stage_env` (pre/main/post) inserts
   the network host env into the delta with `or_insert` when there is no job
   container — after `build_node_env` and before `env.extend(file_cmd_env)`,
   so the file-command paths stay authoritative — so workflow env still wins
   and the export beats the inherited process env.

The context's network env is set by `job_runner` from `RunnerConfig.network`
(no process globals). Docker-action env is built from `build_step_env` without
the process env, so `NODE_EXTRA_CA_CERTS` can never leak into a container
through it.

Containers: the proxy variables (never `NODE_EXTRA_CA_CERTS`) are added with
`entry().or_insert` to the job container's and every service's
`ContainerSpec.env` (`job_http::add_container_network_env`, called from
`job_runner/entry.rs` before any `create_body`) and to the Docker-action env
(`action_container::build_env`, which has `params.config`), so
`container.env`/`services.*.env`/step `env:` win (upstream `TryAdd`). A step
run in the job container (`docker exec`) inherits them from the container's
create env; exec-time env (`container_exec`) adds nothing, so a workflow env
value still overrides per exec.

**Secrets.** `proxy_secrets` returns the proxy password raw and
percent-decoded, and the `Proxy-Authorization` credential
`base64(user:password)`; the bin registers them with the masker that is also
the tracing redactor, and `job_runner` registers them with the job masker, so
a step echoing `$HTTPS_PROXY` is masked and no log line carries them. The
masker ignores values shorter than 4 characters (`MIN_SECRET_LEN`), so such a
password is not maskable on its own — documented; its `base64(user:pass)`
form is always ≥ 4 characters and is still masked. The username is not secret (upstream
parity). `NetworkConfig`'s `Debug` prints the proxy redacted
(`http://user:***@host:port`); every runner-authored message about the proxy
uses the redacted form.

**Diagnostics.** `run` logs one INFO line at startup and whenever the
described settings change between iterations (and registers new proxy
secrets only then); `boot` logs it once: `network: proxy <redacted> (config),
no_proxy <n> entries` / `proxy <redacted> (env)` / `proxy none`, then
`, ca_bundle <path> (<n> certificates)` when set (upstream's "running behind
proxy" line). Env mode reads `HTTPS_PROXY`, then `HTTP_PROXY`, then
`ALL_PROXY`. `check` prints the same line as a separate `[network]` line,
only when `[network]` is non-default.

## Interfaces / Schema

`config.toml` (all keys optional; section optional, omitted on save when empty):

```toml
[network]
proxy     = "http://user:pass@proxy.corp:3128"   # http(s) proxy for http and https destinations
no_proxy  = ["internal.corp", ".svc.local", "10.0.0.0/8"]  # NO_PROXY syntax per entry
ca_bundle = "~/certs/corp-root.pem"               # extra trusted CA(s), PEM; absolute after `~`
```

```rust
// shared::config (pure data, part of RunnerConfig; Debug redacts the proxy)
pub struct NetworkConfig { pub proxy: Option<String>, pub no_proxy: Vec<String>, pub ca_bundle: Option<PathBuf> }
pub struct RunnerConfig { /* … */ pub network: NetworkConfig }
// shared::network_redact
pub fn redact_proxy_url(raw: &str) -> String;

// network (new crate)
pub const LOOPBACK_BYPASS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];
pub fn client_builder(cfg: &NetworkConfig) -> Result<reqwest::ClientBuilder, RunnerError>;
pub fn step_env(cfg: &NetworkConfig) -> StepNetworkEnv;
pub struct StepNetworkEnv { pub host: Vec<(String, String)>, pub container: Vec<(String, String)> }
pub fn proxy_secrets(cfg: &NetworkConfig) -> Vec<String>;
pub fn describe(cfg: &NetworkConfig) -> Result<String, RunnerError>; // the INFO line / check facts

// config crate
pub struct NetworkSection { pub proxy: Option<String>, pub no_proxy: Vec<String>, pub ca_bundle: Option<String> }
impl RunnerRegistrationConfig { pub fn network_config(&self) -> NetworkConfig }

// cache::tier::l2
pub fn L2Tier::from_config(cfg: &L2Config, client: reqwest::Client) -> Result<Self, RunnerError>;

// wire::reporting::live_log
pub async fn LiveLogStreamer::connect(client: &reqwest::Client, url: &str, token: &str) -> Option<(…)>;

// execution
impl ExecutionContext { pub(crate) fn set_network_env(&mut self, host: Vec<(String, String)>); pub(crate) fn inherited_process_env(&self) -> Vec<(String, String)>; }
```

## Failure modes and edge cases

| Input | Observable behaviour |
| --- | --- |
| `[network]` absent / all keys absent / `proxy = ""` | env mode; byte-identical client behaviour to today |
| `proxy` not a URL, no host, or scheme not http/https (`socks5://`) | `RunnerError::Config("[network] proxy …")`, URL redacted; `run` exits 2 before polling; `check --config` exits 2 |
| `proxy` unreachable at runtime | transport error as today (`run` backs off; job steps fail on their own) |
| proxy answers 407 | reqwest error as today; `check` reports `proxy` |
| `ca_bundle` relative | `RunnerError::Config("… must be an absolute path")` |
| `ca_bundle` missing / unreadable / a directory | `RunnerError::Config` naming the path and the io error |
| `ca_bundle` empty / only keys | `RunnerError::Config("… no PEM certificates")` |
| `ca_bundle` with a valid cert plus a malformed PEM block | `RunnerError::Config` naming the path (no partial accept) |
| `ca_bundle` deleted/broken while `run` loops | next iteration exits 2 naming the path (as a corrupt config does); a job already running keeps its clients |
| server cert from a CA in no store / expired leaf / wrong host | TLS error — never accepted, even with `ca_bundle` |
| `no_proxy` entries with spaces / empty entries | trimmed; empty dropped |
| env `HTTPS_PROXY` set **and** `[network] proxy` set | runner clients use config; steps get config values; a `no_proxy` destination goes direct even when env `HTTPS_PROXY` names a dead port |
| workflow sets `HTTPS_PROXY: ''` | step sees `''` (workflow wins) |
| `container.env` / `services.*.env` / Docker-action step `env` sets `HTTPS_PROXY` | that value is kept |
| proxy password ≥ 4 chars | `***` in step logs, journal, `_diag`; config `Debug`/errors redacted |
| proxy password < 4 chars | not maskable by the masker (documented); still redacted in runner-authored messages |
| live-log server rejects upgrade / wrong accept key / TLS failure | WARN, `connect` returns `None`, job runs without live log |

## Acceptance criteria

- **AC-1:** Given a `config.toml` with a `[network]` section, `load_config` →
  `network_config()` yields the `proxy`, trimmed non-empty `no_proxy` entries
  and a `~`-expanded `ca_bundle`; a config without the section yields
  `NetworkConfig::default()` and saves back without a `[network]` table.
- **AC-2:** Given config mode with an authenticated proxy (`user:pass`), a
  `client_builder` client's request to a real HTTPS target goes through a real
  proxy that observes `CONNECT target.test:<port>` with
  `Proxy-Authorization: Basic base64(user:pass)`; a request to a `no_proxy`
  destination reaches its target directly and the proxy records nothing.
- **AC-3:** Given env `HTTPS_PROXY`/`NO_PROXY` naming a second recording proxy
  (or a closed port) and config mode set, only the config proxy sees traffic,
  env `NO_PROXY` is not applied, and a config `no_proxy` destination still
  goes direct; with `proxy` unset the env proxy is used (today's behaviour);
  in config mode a `127.0.0.1` destination is reached directly with no
  `no_proxy`.
- **AC-4:** Given a real HTTPS server whose certificate chains to a generated
  CA: with `ca_bundle` = that CA the request succeeds; without it, with an
  expired leaf, or with a leaf for another host it fails with a TLS error;
  `ca_bundle` relative, missing, a directory, empty, keys-only, or valid+
  malformed returns `RunnerError::Config` naming the path.
- **AC-5:** Given host `run:` steps executed through `Runner::execute_job`
  with `[network]` set, the step's environment contains `HTTPS_PROXY`,
  `https_proxy`, `HTTP_PROXY`, `http_proxy` = the proxy, `NO_PROXY`/`no_proxy`
  = the list plus loopback entries, and `NODE_EXTRA_CA_CERTS` = the bundle
  path; a workflow `env: HTTPS_PROXY` overrides it; a job hook and a composite
  `run:` step see the same values; in env mode a runner env `HTTPS_PROXY`
  reaches the step in both casings; `TOOLU_RUNNER_*` stays stripped.
- **AC-6:** Given a real local Node action (`runs.using: node20`, `main` and
  `post`) executed through the engine with `[network]` set, both stages see
  `HTTPS_PROXY`; a Node `run:` step doing `https.get` against the
  generated-CA server succeeds when `ca_bundle` is set and fails with a
  certificate error when it is not.
- **AC-7:** Given real Docker (Linux lane): a job container's create env, a
  `run:` step executed inside that job container, a service container and a
  Docker action each see the proxy variables in both casings unless their own
  `env` sets them, and none sees `NODE_EXTRA_CA_CERTS`.
- **AC-8:** Given a real `wss://` live-log server behind the authenticated
  proxy with a generated-CA certificate, `LiveLogStreamer::connect` with a
  `client_builder` client tunnels through the proxy, completes the handshake
  and the server receives the sent log frames; without the CA, or when the
  server answers 200 instead of 101, it returns `None`.
- **AC-9:** Given a proxy URL with a password, a step that echoes
  `$HTTPS_PROXY` produces `***` in place of the password in the job's log
  events and journal lines; `proxy_secrets` includes the password (raw and
  decoded) and `base64(user:pass)`; `format!("{:?}")` of the `NetworkConfig`
  and every `[network]` config error show the password as `***`.
- **AC-10:** `toolu-runner check --config <cfg>` with `[network] proxy` routes
  its probes through the configured proxy (observed by a real proxy) even when
  env `HTTPS_PROXY` names a closed port, prints the redacted proxy and the
  `ca_bundle` line, and exits 2 naming the path for a bad `ca_bundle`;
  `toolu-runner run --config <cfg>` with a bad `ca_bundle` exits 2 before
  polling with a message naming the path.
- **AC-11:** No production source file outside `crates/network/src`
  constructs a `reqwest::Client` via `Client::builder()` or `Client::new()`;
  the action archive client fetches a real tarball through the auth proxy with
  the generated CA; an `L2Tier` request is observed by the proxy as
  `CONNECT <s3 host>`.
- **AC-12:** README documents env-mode behaviour, the `[network]` keys,
  precedence, step/container export, secret handling, and the limits in
  Non-Goals 4–7; `docs/test-coverage.md` maps every AC and S1–S4 to tests or
  marks them unverified; AGENTS.md covers the `network` crate and layering.
- **AC-13:** `./tools/check.sh all` passes and `cargo tree -p protocol` still
  contains no `reqwest`.

## Acceptance evidence

All fixtures are generated at test time (no committed keys): rcgen CA + leaves
(`target.test`, `bypass.test`, `s3.test`; one expired; one for `other.test`), a
tokio CONNECT proxy that requires Basic auth, records each request line and
`Proxy-Authorization`, and resolves every requested host to `127.0.0.1`
(portable — no `127.0.0.2`, which macOS lacks), a tokio-rustls HTTPS target and
a TLS WebSocket target. They live once in
`crates/network/tests/helpers/{tls,proxy}_fixtures.rs`, `#[path]`-included by the
other crates' integration tests (the bin crate already uses a crate-root
`tests/helpers/` directory, which the guardrails allow); each including crate
gains `rcgen` (`ring` backend, already in the lock), `tokio-rustls` and, for
`wire`, `tokio-tungstenite` as dev-dependencies. The direct (`bypass.test`)
case pins the name to loopback with `ClientBuilder::resolve`.

| AC | Real input | Expected | Check |
| --- | --- | --- | --- |
| AC-1 | TOML text with and without `[network]`, `~/x.pem`, blank `no_proxy` entries | parsed values / default / round-trip without table | `cargo test -p config --test network_section_test` |
| AC-2 | auth CONNECT proxy + rustls target | proxy log `CONNECT target.test:<port>` + auth header; bypass target direct, proxy log unchanged | `cargo test -p network --test client_proxy_test` |
| AC-3 | two recording proxies / closed port via `temp-env` | only config proxy records; env mode uses env proxy; loopback + config `no_proxy` direct | same |
| AC-4 | rcgen CA, valid / expired / wrong-host leaves; bundle files: relative, missing, dir, empty, key-only, valid+garbage | success vs TLS error; `RunnerError::Config` naming path | `cargo test -p network --test client_tls_test` |
| AC-5 | captured job message (`toolu-runner/tests/fixtures/…`) replayed via `Runner::execute_job` with host `run:`, a composite and a job hook printing env | per-mode values; workflow override; TOOLU_RUNNER_* absent | `cargo test -p execution --test network_step_env_test` |
| AC-6 | temp local node20 action (main+post) + host `node` (required; the test fails, not skips, without it) against the rustls target. The node20 runtime cache is pre-seeded with the host `node` (the existing `seed_node` pattern in `job_outputs_test.rs`/`post_results_test.rs`), so nothing downloads from `nodejs.org` through the fixture proxy and the test needs no internet | `HTTPS_PROXY` in both stages; 200 with bundle, cert error without | same file |
| AC-7 | real Docker: job container (alpine digest already used by `docker_action_linux_test`) + `run:` in it + service + Docker action probe | proxy vars both casings; container/service/step override kept; no `NODE_EXTRA_CA_CERTS` | `cargo test -p execution --test network_container_env_test -- --ignored --test-threads=1` (added to CI's Docker lane) + `cargo test -p execution --lib docker` (pure `create_body`/`build_env`) |
| AC-8 | tokio-tungstenite server over tokio-rustls behind the auth proxy; a 200-answering server | frames received; no-CA / 200 → `None` | `cargo test -p wire --test live_log_proxy_test` |
| AC-9 | step `echo "$HTTPS_PROXY"` with `http://u:s3cr3tpw@…`; journal writer over the events | `***`; Debug/errors redacted; secrets list | `cargo test -p execution --test network_step_env_test` + `cargo test -p network --test secrets_test` |
| AC-10 | real `toolu-runner` binary, temp registration, loopback proxy/targets | proxy observes probes; output lines; exit 2 + path for `check` and `run` | `cargo test -p toolu-runner --test network_cli_test` |
| AC-11 | `crates/*/src`; archive tarball through the proxy; an `L2Tier` put to `https://s3.test:<port>` with static `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` set via `temp-env` (so reqsign signs locally and never probes IMDS) | zero raw constructions; extracted tarball; proxy log `CONNECT s3.test:<port>` — the test asserts the CONNECT, not a successful S3 reply (the fixture target is not S3) | `cargo test -p toolu-runner --test network_client_inventory_test`, `cargo test -p execution --test network_job_clients_test`, `cargo test -p cache --test l2_network_test` |
| AC-12 | docs | sections present, coverage rows | `grep -q '^\[network\]' README.md && grep -q 'Proxy and custom CA (#94)' docs/test-coverage.md && grep -q 'crates/network' AGENTS.md` plus content review in `toolu-review` |
| AC-13 | — | green gate; protocol tree clean | `./tools/check.sh all` + `cargo tree -p protocol` |

S1 → AC-3/AC-5/AC-10; S2 → AC-2/AC-8/AC-9/AC-10; S3 → AC-4/AC-6/AC-8/AC-10;
S4 → AC-5/AC-6/AC-7 plus documented limits. GHES uses the same clients (no
GHES-specific transport). A live GHES-behind-proxy run, a real corporate
proxy, live github.com through a proxy, macOS, and an upstream-runner
comparison are **unverified** and recorded as such in `docs/test-coverage.md`.

## Documentation impact

- `README.md`: Configuration example gains `[network]`; new "Proxy and custom
  CA" section (env mode, config mode, precedence table, step/container export,
  secret masking incl. the 4-character floor, limits: Docker daemon/socket,
  language tools, `boot`/`login`/`create-app`/first `register` env-only, no TLS
  bypass, bundle rules); `check` bullet updated (drop "tracked in #94").
- `docs/test-coverage.md`: new "Proxy and custom CA (#94)" section.
- `AGENTS.md`: crate list/graph gains `network`; module notes for
  `network`, `shared::NetworkConfig`, config `[network]`, live-log transport,
  `L2Tier` client, `job_runner/job_http.rs`.
- `crates/network/README.md` / src READMEs where the guardrail requires them;
  `guardrails.workspace.json` lists the crate.

## Open Questions

None blocking. Decisions (owner: this worker; reasons in the brainstorm record
and above): single `proxy` key; config replaces env proxy + `NO_PROXY`;
loopback added only in config mode; WebSocket via reqwest upgrade;
`NODE_EXTRA_CA_CERTS` host-only; new `network` crate (protocol isolation);
relative `ca_bundle` rejected; invalid bundle mid-run exits 2 like a corrupt
config.

## Spec review record

- Round 1 (independent reviewer): Needs changes — 3 blockers (reqwest in
  `shared` would enter `protocol`'s tree; Node delta env missed the overlay;
  `job_runner.rs` at 491/500 code lines) + 8 should-fix/consider. All applied
  in rev 2 (new `network` crate, `node_stage_env` fold, `job_runner/job_http.rs`,
  bundle rules, secrets, test placement, Docker lane, `run` exit-2 AC).
- Round 2: Needs changes — 1 blocker (node20 runtime download through the
  fixture proxy) + 2 should-fix (hook fold text, L2 credentials/IMDS) + 4
  consider. All applied in rev 3 (`seed_node`, text fix, `temp-env` AWS creds,
  dev-deps, fold order, base64 masking note, runnable doc greps).
- Jev alignment (issue ACs/S1–S4 vs spec evidence): covered 0.76, observable
  0.66 (AC-12/13 are docs/gate criteria). No uncovered requirement found.
- **Status: Approved.**
