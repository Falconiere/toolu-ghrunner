//! The single `daemon` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p daemon --test integration -- docker_live::`.
//!
//! Every test here is `#[ignore]`d live Docker coverage; `daemon-live.yml`
//! selects them all with `cargo test -p daemon -- --ignored`.

mod docker_live;
mod docker_live_budget;
mod docker_live_create;
mod docker_live_destroy;
mod docker_live_reap;
mod docker_live_start;
