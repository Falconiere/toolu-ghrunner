//! The single `protocol` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p protocol --test integration -- app_manifest_test::`.

mod app_jwt_test;
mod app_manifest_test;
mod auth_crt_guard_test;
mod integration;
mod message_crypto;
mod message_types;
