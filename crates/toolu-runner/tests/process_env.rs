//! Integration tests that mutate process-wide state, kept out of the merged
//! `integration` binary (#108).
//!
//! `secret_sink_coverage_test` installs the global tracing subscriber and
//! rewrites `HOME`; `auth_store_test` rewrites `TOOLU_RUNNER_TOKEN`. `temp_env`
//! serializes only its own callers, while other tests spawn `toolu-runner`
//! children that inherit the environment (and `storage_layout_test` reads
//! `HOME`), so these run in their own process. They share one target because
//! they serialize against each other through `temp_env`.

mod auth_store_test;
mod secret_sink_coverage_test;
