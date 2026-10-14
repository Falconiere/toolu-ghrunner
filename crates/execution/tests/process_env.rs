//! Integration tests that mutate process-wide env, kept out of the merged
//! `integration` binary (#109).
//!
//! `network_step_env_test` wraps whole job runs (bash / node steps that
//! inherit the process env) in `temp_env::async_with_vars` setting
//! `HTTPS_PROXY` / `NO_PROXY` / … process-wide. `temp_env` serializes only its
//! own callers, and the other `execution` tests run jobs, spawn steps and build
//! env-mode HTTP clients without it, so this file runs in its own process.

#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

mod network_step_env_test;
