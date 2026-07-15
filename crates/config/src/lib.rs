//! Runner configuration, single-job lockfile, and CLI-login token store.
//!
//! Leaf crate: depends only on `shared` (+ external crates), never on
//! `toolu-runner` or the execution engine.

/// GitHub App identity + secret persistence (`<home>/github-app.json`, 0600).
pub mod app_store;
/// CLI-login bearer-token persistence (0600 file default; keyring opt-in).
pub mod auth_store;
/// Runner registration + runtime config load/save (TOML) and credentials (JSON).
pub mod config;
/// Single-job `fs2` advisory lockfile with stale-lock recovery.
pub mod lockfile;
/// Per-repo runner registration registry (home layout + config resolution).
pub mod registry;
/// Re-mint merge that preserves user-editable config sections verbatim.
pub mod remint;
/// Repo inference from the cwd git remote (pure URL parse + `git` shell-out).
pub mod repo_infer;
/// Supervisor unit rendering (launchd plist / systemd unit) for `install-service`.
pub mod service_unit;
