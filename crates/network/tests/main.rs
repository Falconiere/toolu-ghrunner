//! The single `network` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p network --test integration -- client_proxy_test::`.
//!
//! The `helpers/` fixtures are declared once here; test files reach them with
//! `use crate::<helper>;` (`tests/` must stay flat, so they stay in
//! `helpers/`). The other crates' tests include the same files by `#[path]`.
//! Every proxy-env read in these files runs inside `temp_env` (which
//! serializes its callers process-wide) or ignores the proxy env
//! (`client_tls_test` builds with `.no_proxy()`), so they can share a process.

#[path = "helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "helpers/tls_fixtures.rs"]
mod tls_fixtures;

mod client_proxy_test;
mod client_tls_test;
mod proxy_env_test;
mod secrets_test;
mod step_env_test;
