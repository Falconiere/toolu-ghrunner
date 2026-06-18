//! Live test harness entry point (step 12).
//!
//! This file is the top-level integration-test entry that Cargo compiles
//! when the `live` feature is enabled. The actual tests live under
//! `tests/live/`. Without `--features live` the `mod live;` declaration
//! is dropped, so `tests/live/**` is never even seen by the compiler
//! and `cargo test --workspace` stays hermetic.
//!
//! The tests in `tests/live/` hit the real GitHub API (registration
//! tokens, workflow dispatch, run polling). They require
//! `TOOLU_RUNNER_LIVE_TOKEN` and `TOOLU_RUNNER_LIVE_REPO` to be set
//! at runtime — see `tests/live/harness.rs` for the env contract.
//!
//! Note: the file is named `live_integration.rs` (not `live.rs`) so
//! it does not collide with the `tests/live/` module directory of the
//! same name.

#![cfg(feature = "live")]

mod live;
