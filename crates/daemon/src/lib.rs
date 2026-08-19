//! The toolu compute daemon: turns the `vps_hosts` HTTP contract into
//! sysbox-isolated job containers. See `README.md` for the three invariants the
//! toolu.sh client imposes on this crate.

pub mod adopt;
pub mod auth;
pub mod config;
pub mod docker;
#[cfg(test)]
#[path = "tests/fixture.rs"]
mod fixture;
pub mod gate;
pub mod logging;
pub mod prepull;
pub mod reaper;
pub mod routes;
