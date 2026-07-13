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
/// Per-repo runner registration registry (home layout + config resolution).
pub mod registry;
/// Repo inference from the cwd git remote (pure URL parse + `git` shell-out).
pub mod repo_infer;
