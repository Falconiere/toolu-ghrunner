# network

Explicit proxy and custom-CA networking (#94): the only production
`reqwest::Client` construction in the workspace (`client_builder`), the
proxy/CA variables a job exports to its steps (`step_env`), the proxy
credentials registered with the secret masker (`proxy_secrets`), and the
redacted diagnostic line (`describe`). Depends on `shared` only, so
`protocol` stays free of `reqwest`.

| File | Holds |
| --- | --- |
| `src/lib.rs` | crate docs and re-exports |
| `src/client.rs` | `client_builder`, `describe`, `LOOPBACK_BYPASS`, proxy parsing, `ca_bundle` validation |
| `src/step_env.rs` | `StepNetworkEnv`, `step_env` |
| `src/proxy_env.rs` | `ProxyEnv` (the proxy variables as reqwest uses them), `parse_url`, `redact_url`, `url_secrets` |
| `src/secrets.rs` | `proxy_secrets` |
| `src/config.rs` | the crate's environment reads (the proxy pairs and `REQUEST_METHOD`) |
| `tests/helpers/tls_fixtures.rs` | real rcgen PKI and rustls HTTPS target, `#[path]`-included by other crates' tests |
| `tests/helpers/proxy_fixtures.rs` | recording CONNECT proxy (optional Basic auth), included beside `tls_fixtures` |
