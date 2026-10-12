//! Explicit proxy and custom-CA networking for every runner HTTP client (#94).
//!
//! The one place production code constructs a `reqwest::Client` builder
//! ([`client_builder`]), plus what a job exports to its steps
//! ([`step_env`]), which proxy credentials the secret masker must learn
//! ([`proxy_secrets`]), and the redacted one-line diagnostic ([`describe`]).
//!
//! Two modes, decided by [`shared::NetworkConfig::proxy`]:
//! - **env mode** (unset): clients keep reqwest's system proxy
//!   (`HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY`, upper case
//!   first, `REQUEST_METHOD` disables them);
//! - **config mode** (set): the configured proxy for http and https
//!   destinations, bypassing `no_proxy` plus [`LOOPBACK_BYPASS`]; the proxy
//!   environment variables are ignored.
//!
//! `ca_bundle` adds trust anchors in both modes and never removes any. This
//! crate sits below `wire`, `cache`, `execution`, `listener` and the bin, and
//! above `shared` only, so `protocol` stays free of `reqwest`.

mod client;
mod config;
mod secrets;
mod step_env;

pub use client::{LOOPBACK_BYPASS, client_builder, describe};
pub use secrets::proxy_secrets;
pub use step_env::{StepNetworkEnv, step_env};
