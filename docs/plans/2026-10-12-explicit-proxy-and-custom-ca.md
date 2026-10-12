# Explicit proxy and custom CA configuration — Plan

**Date:** 2026-10-12   **Status:** Approved   **Spec:** docs/specs/2026-10-12-explicit-proxy-and-custom-ca-design.md   **Topic:** issue #94 — `[network]` config for every HTTP client, exported to steps

## Evidence and approach

Inspected: the 10 `reqwest::Client::builder()` sites and the one
`Client::new()` (`job_runner.rs` accelerated upstream); `fetch_tarball_reader`
ignores its client and `request_archive` builds a private no-redirect client;
`oidc::server` holds a process-global `OnceLock` client (and `OidcServer` is
not started in production); `L2Tier::from_config` is only constructed in
tests; `LiveLogStreamer::connect` uses `tokio_tungstenite::connect_async`.
Host env: every host builder starts from `ExecutionContext::build_step_env` /
`build_host_step_env` and folds `context::safe_process_env_vars` last with
`or_insert` (`script_support.rs`, `composite_env.rs`, `job_hooks.rs`), Node
inherits it in `node_exec::build_node_command` under a delta env from
`node_stage_env`. Containers: `job_container::create_body`,
`service_create::create_body`, `action_container::build_env` (has
`params.config`). reqwest 0.12.28: `.proxy()` disables the system proxy;
`Proxy::all` has no implicit `NO_PROXY`; `NoProxy::from_string` uses
hyper-util's matcher; `Certificate::from_pem_bundle`; `Response::upgrade`;
`http1_only`. opendal 0.51 `S3::http_client(HttpClient::with(reqwest::Client))`.
`ring`, `tokio-rustls 0.26`, `temp-env 0.3` are already in the lock; `rcgen`
is new (dev-only). comemory 99bd07ca (trust union, matcher order),
43e16a1f/785b43e8 (job lease, separate target dirs for base builds).

Approach: the spec's single builder in the new `network` crate (`protocol` depends on `shared`, so `reqwest` cannot go there); the network export
is an overlay on the **inherited** host layer (`ExecutionContext::
inherited_process_env`, replacing direct `safe_process_env_vars` folds) so it
never leaks into Docker-action env (which is built from `build_step_env`
without the process env) and stays below workflow env. Archive downloads get a
dedicated no-redirect archive client owned by `ActionFetcher` (built from the
job's `NetworkConfig`). Test fixtures (rcgen CA/leaves, a tokio CONNECT proxy
that requires Basic auth and resolves every host to loopback, a rustls HTTPS
target, a TLS WebSocket target) live once in
`crates/network/tests/helpers/network_fixtures.rs` and are `#[path]`-included by
the other crates' integration tests.

## Workstream summary

network crate + fixtures → config section → bin clients (run/remove/check/
boot/register/login/create-app) → execution clients (job, archive, OIDC,
accelerated, L2) → step/container export → live-log WebSocket → inventory pin
→ docs → gate.

## Steps (machine-readable)

```json
[
  {
    "id": "network_crate",
    "title": "New crates/network crate (shared + reqwest rustls-native-roots + base64 + percent-encoding + tracing): client_builder (config-mode Proxy::all + NoProxy incl. LOOPBACK_BYPASS, ca_bundle absolute/readable/PEM rules), step_env (env reads in network/src/config.rs), proxy_secrets, describe; shared gains NetworkConfig (RunnerConfig.network, redacting Debug) + network_redact::redact_proxy_url; guardrails config + workspace manifest; fixtures helper (rcgen CA/leaves, auth CONNECT proxy resolving to loopback, rustls HTTPS target, TLS WS target); integration tests",
    "ac_refs": [
      "AC-2",
      "AC-3",
      "AC-4",
      "AC-9",
      "AC-13"
    ],
    "paths": [
      "Cargo.lock",
      "crates/network",
      "crates/shared/Cargo.toml",
      "crates/shared/src/lib.rs",
      "crates/shared/src/config.rs",
      "crates/shared/src/network_redact.rs",
      "crates/shared/tests/network_redact_test.rs",
      "guardrails.workspace.json"
    ],
    "input": "rcgen CA and leaves (target.test, bypass.test, expired, other.test); auth CONNECT proxy requiring u:p%40ss; temp-env HTTPS_PROXY/NO_PROXY incl. a closed port; bundle files: relative, missing, directory, empty, key-only, valid+malformed.",
    "check": "cargo test -p network && cargo test -p shared --test network_redact_test && ! cargo tree -p protocol | grep -q reqwest"
  },
  {
    "id": "config_network",
    "title": "config: NetworkSection in crates/config/src/network.rs, RunnerRegistrationConfig.network (serde default, skip when empty), network_config() (trim/drop empty no_proxy, expand_tilde ca_bundle)",
    "ac_refs": [
      "AC-1"
    ],
    "depends_on": [
      "network_crate"
    ],
    "paths": [
      "crates/config/Cargo.toml",
      "crates/config/src/lib.rs",
      "crates/config/src/config.rs",
      "crates/config/src/network.rs",
      "crates/config/tests/network_section_test.rs"
    ],
    "input": "config.toml text with and without [network]; ca_bundle ~/certs/x.pem; no_proxy with blank entries.",
    "check": "cargo test -p config --test network_section_test && cargo test -p config"
  },
  {
    "id": "bin_clients",
    "title": "toolu-runner: run loop rebuilds its client from the reloaded [network] each iteration (exit 2 on error), registers proxy_secrets with the masker, logs describe() INFO; build_runner_config sets network; remove uses registration network; boot logs describe + uses default; register/login/create-app use client_builder(default); check probe client from the registration network + [network] report lines (redacted); shell-out tests for check and run",
    "ac_refs": [
      "AC-10",
      "AC-9"
    ],
    "depends_on": [
      "config_network"
    ],
    "paths": [
      "crates/toolu-runner/Cargo.toml",
      "crates/toolu-runner/src",
      "crates/toolu-runner/tests/network_cli_test.rs",
      "crates/network/tests/helpers/network_fixtures.rs"
    ],
    "input": "Temp registration config.toml with [network] proxy=http://u:secretpw@127.0.0.1:<proxy> + ca_bundle; env HTTPS_PROXY=http://127.0.0.1:<closed>; a bad ca_bundle path; the real toolu-runner binary (check and run).",
    "check": "cargo test -p toolu-runner --test network_cli_test && cargo test -p toolu-runner --test check_cmd_test && cargo test -p toolu-runner --bin toolu-runner"
  },
  {
    "id": "execution_clients",
    "title": "execution/cache: move build_job_client + accelerated upstream client into job_runner/http.rs (network-built); ActionFetcher owns a no-redirect archive client from the job network, download_and_extract_action uses it; OidcState client from OidcConfig.network (no global OnceLock); L2Tier::from_config(cfg, client) via S3::http_client; real archive download via auth proxy + CA; L2 put observed by proxy",
    "ac_refs": [
      "AC-11",
      "AC-2",
      "AC-4"
    ],
    "depends_on": [
      "network_crate"
    ],
    "paths": [
      "crates/execution/Cargo.toml",
      "crates/execution/src/execution/job_runner.rs",
      "crates/execution/src/execution/job_runner",
      "crates/execution/src/execution/actions",
      "crates/execution/src/execution/oidc",
      "crates/cache/Cargo.toml",
      "crates/cache/src/tier/l2.rs",
      "crates/cache/tests/l2_network_test.rs",
      "crates/toolu-runner/tests/cache_l2_test.rs",
      "crates/execution/tests/network_job_clients_test.rs",
      "crates/execution/tests/action_downloads_test.rs",
      "crates/network/tests/helpers/network_fixtures.rs"
    ],
    "input": "A real gzip tarball served as https://target.test:<port>/a.tar.gz by the rustls target behind the auth proxy with the rcgen CA (and without the CA \u2192 TLS failure); an L2Tier put to https://s3.test:<port> with static AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY via temp-env (assert the proxy CONNECT, not an S3 success).",
    "check": "cargo test -p execution --test network_job_clients_test && cargo test -p execution --lib actions && cargo test -p execution --test action_downloads_test && cargo test -p cache --test l2_network_test && cargo test -p toolu-runner --test oidc_token_test && cargo test -p toolu-runner --test cache_l2_test"
  },
  {
    "id": "step_export",
    "title": "execution: ExecutionContext network host env + inherited_process_env used by script_support/composite_env/job_hooks; node_stage_env or_insert (host only); job_runner sets it from config.network + registers proxy_secrets with the job masker; container proxy vars or_insert in job_container/service_create/action_container (never NODE_EXTRA_CA_CERTS); engine tests (host run, composite, hook, node action main/post, node CA) + Docker lane test (#[ignore], added to CI Docker lane)",
    "ac_refs": [
      "AC-5",
      "AC-6",
      "AC-7",
      "AC-9"
    ],
    "depends_on": [
      "execution_clients"
    ],
    "paths": [
      "crates/execution/src",
      "crates/execution/tests/network_step_env_test.rs",
      "crates/execution/tests/network_container_env_test.rs",
      ".github/workflows/ci.yml",
      "scripts/test/ci_workflow_test.sh",
      "crates/network/tests/helpers/network_fixtures.rs"
    ],
    "input": "The captured job message fixture replayed through Runner::execute_job with host run:/composite/hook steps printing env, a workflow env override and an echo of a password-bearing $HTTPS_PROXY whose events also pass through the real observability journal writer (dev-dep); a temp local node20 action (main+post) with the node20 runtime cache pre-seeded from the host node (seed_node pattern, no internet); host node https.get against the rustls target with/without ca_bundle; real Docker job container + run: inside it + alpine service + Docker action printing env.",
    "check": "cargo test -p execution --test network_step_env_test && cargo test -p execution --lib docker && cargo test -p execution --test network_container_env_test -- --ignored --test-threads=1 && bash scripts/test/ci_workflow_test.sh"
  },
  {
    "id": "live_log",
    "title": "wire: LiveLogStreamer::connect(client, url, token) via HTTP/1.1 Upgrade + WebSocketStream::from_raw_socket (101 + accept check, 30s bound, drop tungstenite TLS feature); listener builds an http1_only client from runner_cfg.network; real wss server behind auth proxy with rcgen CA, 200-answering server",
    "ac_refs": [
      "AC-8"
    ],
    "depends_on": [
      "network_crate"
    ],
    "paths": [
      "crates/wire/Cargo.toml",
      "crates/wire/src/reporting/live_log.rs",
      "crates/wire/tests",
      "crates/listener/Cargo.toml",
      "crates/listener/src",
      "crates/network/tests/helpers/network_fixtures.rs"
    ],
    "input": "tokio-tungstenite server accepting over tokio-rustls (rcgen leaf for target.test) reached through the auth CONNECT proxy; a server that answers 200; no ca_bundle.",
    "check": "cargo test -p wire && cargo test -p listener"
  },
  {
    "id": "inventory",
    "title": "Source-inventory test: no production file under crates/*/src (excluding tests/ dirs and crates/daemon) constructs reqwest::Client::builder()/Client::new() outside crates/network/src",
    "ac_refs": [
      "AC-11"
    ],
    "depends_on": [
      "bin_clients",
      "execution_clients",
      "live_log"
    ],
    "paths": [
      "crates/toolu-runner/tests/network_client_inventory_test.rs",
      "crates"
    ],
    "input": "The repository's own crates/*/src tree.",
    "check": "cargo test -p toolu-runner --test network_client_inventory_test"
  },
  {
    "id": "docs",
    "title": "README [network] + 'Proxy and custom CA' section + check bullet; docs/test-coverage.md #94 section (AC/S1-S4, unverified lanes); AGENTS.md crate list/graph + module notes; crate READMEs",
    "ac_refs": [
      "AC-12"
    ],
    "depends_on": [
      "step_export",
      "live_log",
      "bin_clients"
    ],
    "paths": [
      "README.md",
      "docs/test-coverage.md",
      "AGENTS.md",
      "crates/network/README.md"
    ],
    "input": "The implemented behaviour and test names.",
    "check": "grep -q '^\\[network\\]' README.md && grep -q 'Proxy and custom CA (#94)' docs/test-coverage.md && grep -q 'crates/network\\|`network`' AGENTS.md"
  },
  {
    "id": "gate",
    "title": "Full quality gate + protocol isolation",
    "ac_refs": [
      "AC-13"
    ],
    "depends_on": [
      "inventory",
      "docs",
      "step_export"
    ],
    "paths": [
      "crates",
      "Cargo.toml",
      "Cargo.lock",
      "README.md",
      "docs",
      "AGENTS.md",
      "clippy.toml",
      "tools",
      "scripts",
      "guardrails.workspace.json",
      ".github"
    ],
    "input": "The whole workspace.",
    "check": "./tools/check.sh all && ! cargo tree -p protocol | grep -qE 'reqwest|tokio|opendal|bollard|axum'"
  }
]
```

## Critical files

- Create: `crates/network/` (`Cargo.toml`, `guardrails.config.json`,
  `README.md`, `src/{lib,config,client,step_env,secrets}.rs`,
  `tests/{client_proxy_test,client_tls_test,secrets_test,step_env_test}.rs`,
  `tests/helpers/network_fixtures.rs`), `crates/shared/src/network_redact.rs`,
  `crates/config/src/network.rs`, `crates/config/tests/network_section_test.rs`,
  `crates/toolu-runner/tests/network_cli_test.rs`,
  `crates/toolu-runner/tests/network_client_inventory_test.rs`,
  `crates/execution/src/execution/job_runner/http.rs`,
  `crates/execution/tests/network_job_clients_test.rs`,
  `crates/execution/tests/network_step_env_test.rs`,
  `crates/execution/tests/network_container_env_test.rs`,
  `crates/cache/tests/l2_network_test.rs`,
  `crates/wire/tests/live_log_proxy_test.rs`.
- Modify: client sites listed in the spec inventory; `guardrails.workspace.json`;
  `.github/workflows/ci.yml` Docker lane; `shared/src/config.rs`,
  `config/src/config.rs`, `execution/src/execution/{context,context_env,
  composite_env,job_hooks,node_stage}.rs`, `steps_runner/script_support.rs`,
  `docker/{job_container,service_create,action_container}.rs`,
  `wire/src/reporting/live_log.rs`, `listener/src/execution_loop.rs`,
  `cache/src/tier/l2.rs`, Cargo manifests, README, AGENTS.md,
  `docs/test-coverage.md`.

## Verification

End to end: a registration with `[network]` drives `check` through a real
proxy (bin), real host/Node/Docker steps through the engine see the exported
values with secrets masked, the live log tunnels through the proxy with the
custom CA, and every bad input fails closed with a path-naming
`RunnerError::Config`. Boundary inputs: empty/absent section, env-vs-config
conflict, workflow/container overrides, missing/empty/garbage bundle,
expired/wrong-host certs, password in the proxy URL. Docs are synchronized
in the `docs` step; `./tools/check.sh all` (through `job.ts`) closes it.
Docker/node-dependent tests fail loudly (not skip-as-pass) when the tool is
absent, matching the repo's existing live lanes, or are recorded unverified.

Delivery: scoped commits per step (WIP subjects allowed, squash-merged);
`plan-ledger.js run <plan> --verify` on the final branch; `toolu-review:review`
(version 2 state) → `verdict.js status` = ready → push
`feat/94-explicit-proxy-and-custom-ca` → PR to `main` (`Closes
Falconiere/toolu-ghrunner#94`, `Part of Falconiere/toolu-ghrunner#67`) →
`pr-babysit:babysit`.

## Plan review record

- AC coverage: every spec AC-1..13 maps to ≥1 step; no dangling refs; every
  `depends_on` precedes its dependant (scripted check).
- Fixed in review: the `run` exit-2 test lives in `network_cli_test.rs` (was
  misnamed `check_network_test`); AC-9 journal assertion uses the real
  `observability` writer as an `execution` dev-dep; delivery sequence added.
- Docker AC-7 runs locally with `--ignored` (Docker 29 present) and joins CI's
  existing Docker lane; Node 24 present; no internet needed by any new test.
- **Status: Approved.**

