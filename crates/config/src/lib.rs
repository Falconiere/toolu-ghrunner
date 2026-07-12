//! Runner configuration, single-job lockfile, and CLI-login token store.
//!
//! Leaf crate: depends only on `shared` (+ external crates), never on
//! `toolu-runner` or the execution engine.

/// CLI-login bearer-token persistence (keyring / 0600-file fallback).
pub mod auth_store;
/// Runner registration + runtime config load/save (TOML) and credentials (JSON).
pub mod config;
/// Single-job `fs2` advisory lockfile with stale-lock recovery.
pub mod lockfile;
