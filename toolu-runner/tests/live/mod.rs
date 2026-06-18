//! Live test harness — module root.
//!
//! The parent file `tests/live.rs` is the only entry that Cargo compiles
//! when the `live` feature is on. This `mod.rs` is the module body for
//! `mod live;` declared in that file. Everything below is gated by the
//! same feature flag — no `#[cfg(feature = "live")]` needed on the
//! submodules because the parent declaration is conditional.
//!
//! Layout:
//! - [`harness`] — `LiveHarness` struct + GitHub API helpers + binary
//!   build/spawn helpers. Shared by every test in this directory.
//! - [`register_test`] — register flow (AC #1a, #1b).
//! - [`run_test`] — end-to-end `run --once` flow (AC #2–#5, #13, #14).

pub mod harness;
pub mod register_test;
pub mod run_test;
