//! The single `wire` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p wire --test integration -- app_manifest_test::`.
//!
//! `proxy_fixtures` / `tls_fixtures` come from `../../network/tests/helpers/`,
//! shared with that crate's tests; `live_log_proxy_test` reaches them with
//! `use crate::<helper>;`.

#[path = "../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

mod app_manifest_test;
mod broker_poll_parser;
mod completejob_wire_test;
mod device_token_debug_test;
mod github_app_test;
mod job_outputs_test;
mod live_log_proxy_test;
mod oauth_invalid_client;
mod runner_version;
mod session_conflict;
mod toolu_jobs_wire_test;
mod toolu_pair_wire_test;
