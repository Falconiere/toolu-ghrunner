# `toolu-runner check` — Plan

**Date:** 2026-10-12   **Status:** Approved   **Spec:** docs/specs/2026-10-12-check-diagnostics-design.md   **Topic:** issue #93 — operator preflight diagnostics subcommand

## Evidence and approach

- Upstream host set and GHES paths: actions/runner `cab9d1c`
  `src/Runner.Listener/Checks/ActionsCheck.cs` / `InternetCheck.cs` (read via
  `gh api`). Proxy semantics: hyper-util 0.1.21 `Matcher::from_system`
  (ALL_/HTTP_/HTTPS_/NO_PROXY, upper first, CGI `REQUEST_METHOD` guard), used
  unconditionally by reqwest 0.12.28 `ClientBuilder` (`auto_sys_proxy`).
  Trust: rustls-native-certs 0.8.4 `load_native_certs` (SSL_CERT_FILE /
  SSL_CERT_DIR replace the platform store).
- Reused: `crate::resolve_config` (main.rs), `config::config::load_config`,
  `protocol::JitConfig::parse`, `shared::SecretMasker`, the `boot` explicit
  exit-code dispatch in `main.rs`, `crate::config::read` for env access
  (`no-direct-env-var` guardrail), the `CARGO_BIN_EXE_toolu-runner` shell-out
  test style of `tests/boot_cmd_test.rs`, axum (already a dev-dep) for the
  local target / proxy servers.
- The bin crate's guardrail config allows no nested `src/` folders except
  `tests/`, so the module is flat `check_*.rs` files; pure units are tested
  through `src/tests/check_*.rs` includes (bin-only crate, no lib target).
- Recalled: epic job-lease gotchas (memory 43e16a1f, ced17f92) — full gate and
  `--verify` go through `job.ts -- --` and are retried on a refused lease.

## Workstream summary

pure env/redaction → pure endpoint derivation → HTTP probes + report +
command wiring with local-server shell-out tests → tool probes → live
github.com evidence → docs → full gate.

## Steps (machine-readable)

```json
[
  {
    "id": "check_env",
    "title": "Env accessors in config.rs (proxy vars, REQUEST_METHOD, SSL_CERT_FILE/DIR, NODE_EXTRA_CA_CERTS, DOCKER_HOST) + check_env.rs: redact_url, ProxyReport (first-set upper>lower), TrustReport via rustls_native_certs::load_native_certs, masker seeding; unit tests",
        "paths": [
      "Cargo.lock",
      "crates/toolu-runner/Cargo.toml",
      "crates/toolu-runner/src/config.rs",
      "crates/toolu-runner/src/check_env.rs",
      "crates/toolu-runner/src/tests/check_env.rs",
      "crates/toolu-runner/src/main.rs"
    ],
    "input": "Proxy/DOCKER_HOST strings with and without userinfo, with path/query, unparseable values; the committed ISRG Root X1 PEM.",
    "check": "cargo test -p toolu-runner --bin toolu-runner check_env"
  },
  {
    "id": "check_endpoints",
    "title": "check_endpoints.rs: Target (Url | Registration | Defaults), endpoints_for() per the spec table (github.com, GHES, IP-literal codeload skip, registration ServerUrl/ServerUrlV2/AuthorizationUrl overrides, GHES broker skip); unit tests",
        "paths": [
      "crates/toolu-runner/src/check_endpoints.rs",
      "crates/toolu-runner/src/tests/check_endpoints.rs",
      "crates/toolu-runner/src/main.rs"
    ],
    "input": "The committed github.com and GHES JIT fixtures (tests/fixtures/jit_config_*.json) parsed with protocol::JitConfig::parse; https://github.com/o/r, https://ghes.example.com/o, http://127.0.0.1:8080.",
    "check": "cargo test -p toolu-runner --bin toolu-runner check_endpoints"
  },
  {
    "id": "check_command",
    "title": "check_probe.rs (bounded GET, any-response pass except 407, error classification), check_report.rs (render through SecretMasker, exit_code), check_cmd.rs (target resolution, JoinSet, exit code), CheckArgs in cli.rs, main.rs dispatch; shell-out tests against real local servers",
    "ac_refs": ["AC-1", "AC-2", "AC-3", "AC-4", "AC-5", "AC-6", "AC-8", "AC-9", "AC-12", "AC-13"],
    "depends_on": ["check_env", "check_endpoints"],
    "paths": [
      "crates/toolu-runner/src/check_probe.rs",
      "crates/toolu-runner/src/check_report.rs",
      "crates/toolu-runner/src/check_cmd.rs",
      "crates/toolu-runner/src/check_env.rs",
      "crates/toolu-runner/src/check_endpoints.rs",
      "crates/toolu-runner/src/cli.rs",
      "crates/toolu-runner/src/main.rs",
      "crates/toolu-runner/src/tests/check_report.rs",
      "crates/toolu-runner/tests/check_cmd_test.rs",
      "crates/toolu-runner/tests/helpers/check_servers.rs",
      "crates/toolu-runner/tests/fixtures/check/isrg-root-x1.pem",
      "crates/toolu-runner/tests/fixtures/jit_config_ghes.json"
    ],
    "input": "The real toolu-runner binary against: an axum server on 127.0.0.1 recording method+path (200 / 401 routes); a TcpListener that accepts and never answers; a bound-then-dropped port; https:// to the plaintext server; toolu-check.invalid; a recording forward proxy (absolute-form + Proxy-Authorization capture, CONNECT recording + close, 407 variant); a temp registration whose JIT envelope points at the local server; the ISRG Root X1 PEM; an empty TOOLU_RUNNER_HOME; an fs2-held .lock.",
    "check": "cargo test -p toolu-runner --bin toolu-runner check_ && cargo test -p toolu-runner --test check_cmd_test"
  },
  {
    "id": "check_tools",
    "title": "check_tools.rs: git/docker/node via tokio::process (kill_on_drop, timeout), docker endpoint line (redacted DOCKER_HOST), node cached runtimes under data_dir/node; wired into the report; shell-out tests with the host's real binaries",
    "ac_refs": ["AC-7"],
    "depends_on": ["check_command"],
    "paths": [
      "crates/toolu-runner/src/check_tools.rs",
      "crates/toolu-runner/src/check_cmd.rs",
      "crates/toolu-runner/src/check_report.rs",
      "crates/toolu-runner/src/main.rs",
      "crates/toolu-runner/tests/check_tools_test.rs",
      "crates/toolu-runner/tests/helpers/check_servers.rs"
    ],
    "input": "A healthy local axum target (helpers/check_servers.rs) so the exit code isolates tool effects; the host's real git / docker / node binaries (versions captured by running them in the test), an empty-dir PATH, a `git` shell script that sleeps 30 s, DOCKER_HOST=unix:///<tmp>/absent.sock and tcp://user:<secret>@127.0.0.1:<closed>.",
    "check": "cargo test -p toolu-runner --test check_tools_test"
  },
  {
    "id": "check_live",
    "title": "Live github.com test (feature `live`, #[ignore]): all six required probes PASS; narrowed SSL_CERT_FILE makes them FAIL tls. Run manually; output recorded in docs/test-coverage.md and the PR",
    "ac_refs": ["AC-10"],
    "depends_on": ["check_tools"],
    "paths": [
      "crates/toolu-runner/tests/check_cmd_test.rs",
      "crates/toolu-runner/tests/helpers/check_servers.rs",
      "crates/toolu-runner/src/check_cmd.rs"
    ],
    "input": "Real github.com / *.actions.githubusercontent.com from this networked host; the committed ISRG Root X1 PEM as the only trust anchor.",
    "check": "cargo test -p toolu-runner --features live --test check_cmd_test -- --ignored live_github_com"
  },
  {
    "id": "docs",
    "title": "README (check usage + exit codes), AGENTS.md (toolu-runner module list + check rule), docs/test-coverage.md (#93 AC/scenario map incl. unverified lanes), docs/architecture.md if it lists subcommands, cli.rs Examples footer",
    "depends_on": ["check_live"],
    "paths": [
      "README.md",
      "AGENTS.md",
      "docs/test-coverage.md",
      "docs/architecture.md",
      "crates/toolu-runner/src/cli.rs"
    ],
    "input": "The implemented CLI's --help output and the passing test names.",
    "check": "grep -q 'toolu-runner check' README.md && grep -q 'check_cmd.rs' AGENTS.md && grep -q '#93' docs/test-coverage.md && grep -q 'toolu-runner check' crates/toolu-runner/src/cli.rs"
  },
  {
    "id": "gate",
    "title": "Full repository gate",
    "ac_refs": ["AC-11"],
    "depends_on": ["docs"],
    "paths": ["."],
    "input": "The whole workspace.",
    "check": "./tools/check.sh all"
  }
]
```

## Critical files

- Create: `crates/toolu-runner/src/check_cmd.rs`, `check_endpoints.rs`,
  `check_probe.rs`, `check_tools.rs`, `check_env.rs`, `check_report.rs`;
  `crates/toolu-runner/src/tests/check_env.rs`, `check_endpoints.rs`,
  `check_report.rs`; `crates/toolu-runner/tests/check_cmd_test.rs`,
  `check_tools_test.rs`, `helpers/check_servers.rs`;
  `crates/toolu-runner/tests/fixtures/check/isrg-root-x1.pem`.
- Modify: `crates/toolu-runner/src/{main.rs,cli.rs,config.rs}`,
  `crates/toolu-runner/Cargo.toml`, `Cargo.lock`, `README.md`, `AGENTS.md`,
  `docs/test-coverage.md`, possibly `docs/architecture.md`.

## Verification

- End to end: the real binary against real local HTTP servers/proxies proves
  probing, bounding, classification, redaction, read-only behaviour and exit
  codes (AC-1..9, 12, 13); real host tools prove AC-7; the live github.com run
  proves AC-10 and that the CA setting is effective.
- Failure/boundary: hang, DNS, refused, TLS, 407, CONNECT refused, bad
  args/config, missing/hung tools, missing CA bundle, held `.lock`.
- Unverified lanes recorded, not counted: GHES live; docker-daemon cases on a
  host without the docker CLI (macOS CI).
- Docs synchronized in the `docs` step; full gate in `gate`.
- Delivery: scoped conventional commits; final
  `plan-ledger.js run <plan> --verify` (retried on refused leases);
  `toolu-review:review` with version-2 push state; `verdict.js status` →
  `overall: ready`; push `feat/93-add-a-check-diagnostics-subcommand`; PR to
  `main` starting `Closes Falconiere/toolu-ghrunner#93` / `Part of
  Falconiere/toolu-ghrunner#67`; `pr-babysit:babysit`.

## Plan review (2026-10-12)

Round 1 — Needs changes:
- Steps: 🔴 blocker: `tests/check_support/mod.rs` would be a banned `mod.rs` barrel. Moved to `tests/helpers/check_servers.rs` (existing `#[path]` helper convention).
- check_live: 🔴 blocker: `--no-run` cannot prove AC-10. Check now runs the ignored live test.
- check_env / check_endpoints / docs: 🟡 should-fix: `ac_refs` cited ACs their unit/grep checks cannot prove (Jev misaligned 0.86). Refs removed; those ACs map to the shell-out steps.
- check_tools: 🟡 should-fix: AC-7's "exit code unaffected" needs a healthy target. Input now names the local axum target.
- Verification: 🟡 should-fix: delivery sequence absent. Added.

Round 2 — Approved: every spec AC maps to a step (jq/comm diff empty); Jev missing-failure 0.27. The residual Jev pick (`gate`, confidence 0.37) is a false positive — AC-11 is defined as `./tools/check.sh all`, which is that step's check.

## Deviations

- CGI guard: hyper-util's `REQUEST_METHOD` check disables **all** proxy
  variables, not only `HTTP_PROXY` (read in hyper-util 0.1.21
  `Builder::build`). Spec AC-5 / architecture wording corrected.
- Default broker probe is `https://broker.actions.githubusercontent.com/health`
  (live: `/` answers 404, `/health` 200), matching the other health paths.
- `redact_url` keeps the path only for host-less socket URLs
  (`unix:///var/run/docker.sock`) so the Docker endpoint stays meaningful.
- A registration resolved implicitly (not via `--config`) that fails to load
  also exits 2 — a broken registration is a configuration error, not "no
  registration".
- The defaults header carries `RunnerError`'s `config error:` prefix in the
  reason; the AC-13 assertion matches the real text.
- **Trust is native ∪ bundled.** The live AC-10 run with an ISRG-only
  `SSL_CERT_FILE` still verified github.com: `cargo tree -e features -i reqwest`
  shows `opendal`'s default `rustls-tls` unifying on
  `rustls-tls-webpki-roots`, and reqwest 0.12.28 adds
  `webpki_roots::TLS_SERVER_ROOTS` alongside the native anchors. `check` now
  reports the bundled set, the zero-anchor case no longer claims every TLS
  probe fails, the bin's manifest names the feature explicitly, and AC-10's
  narrowed run asserts PASS + the bundled line. (Input for #94: the env vars
  cannot narrow trust today.)
- The probe client sends `User-Agent: toolu-runner/<ver> (check)`; without
  one `api.github.com` answered 403 (live).
