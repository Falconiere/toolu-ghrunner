//! The single `shared` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p shared --test integration -- config_test::`.
//!
//! `startup_redaction_test` drives `RedactingWriter` directly and never
//! installs the global tracing subscriber, so it shares this process.

mod config_test;
mod error_test;
mod events_test;
mod job_message_test;
mod network_config_debug_test;
mod platform_test;
mod redacting_writer_test;
mod secret_masker_test;
mod startup_redaction_test;
mod template_token_test;
