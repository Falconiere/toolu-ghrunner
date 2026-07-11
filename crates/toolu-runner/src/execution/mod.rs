//! Job execution engine.
//!
//! Populated in steps 4c + 4d.

pub mod action_exec;
mod action_support;
pub mod actions;
pub mod artifacts;
pub mod cache;
pub mod cgroup_join;
pub mod command_dispatch;
pub mod command_parser;
mod composite_env;
pub mod composite_exec;
pub mod composite_expr;
pub mod composite_scope;
pub mod composite_shell;
mod composite_uses;
pub mod context;
pub(crate) mod context_build;
pub mod depth_tracker;
pub mod expressions;
pub mod file_commands;
pub mod handlers;
pub mod job_hooks;
pub mod job_runner;
pub mod job_spec;
mod node_stage;
pub mod oidc;
mod post_drain;
mod service_auth;
pub mod service_endpoints;
pub mod service_lifecycle;
/// Shadow-mode step observation (approach C): records only, never serves.
pub mod shadow;
mod step_env;
pub mod step_host;
mod step_naming;
mod step_state;
pub mod step_timeout;
pub mod steps_runner;
pub mod workflow;
pub mod workspace_gc;
