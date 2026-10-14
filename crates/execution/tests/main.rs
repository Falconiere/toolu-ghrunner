//! The single `execution` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p execution --test integration -- action_downloads_test::`.
//!
//! `network_step_env_test` sets proxy env vars process-wide around whole job
//! runs, so it lives in the separate `process_env` target
//! (`tests/process_env.rs`).
//!
//! Shared helpers are declared once here; test files reach them with
//! `use crate::<helper>;`. `proxy_fixtures` / `tls_fixtures` come from
//! `../../network/tests/helpers/`, shared with that crate's tests.

#[path = "helpers/orphan_replay.rs"]
mod orphan_replay;
#[path = "../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

mod action_downloads_test;
mod action_infrastructure_test;
mod action_ref_traversal_test;
mod command_annotation_test;
mod command_dispatch_guard_test;
mod compat_probe_replay_test;
mod composite_bounds_test;
mod composite_runner_context_test;
mod composite_runner_temp_test;
mod composite_semantics_test;
mod conditional_cleanup_test;
mod context_secret_batch_test;
mod docker_action_linux_test;
mod docker_action_test;
mod env_isolation_test;
mod environment_url_test;
mod expression_parity_test;
mod file_command_guard_test;
mod file_command_strict_test;
mod incoming_contexts_node_test;
mod incoming_contexts_test;
mod interpolate_fast_path_test;
mod job_container_capture_replay;
mod job_container_ci_test;
mod job_container_composite_test;
mod job_container_linux_test;
mod job_container_test;
mod job_message_run_defaults_test;
mod job_outputs_test;
mod literal_status_condition_test;
mod network_container_env_test;
mod network_job_clients_test;
mod node_container_test;
mod node_download_test;
mod node_migration_test;
mod oidc_url_test;
mod orphan_cleanup_container_test;
mod orphan_cleanup_test;
mod post_results_test;
mod replay_mutation_test;
mod runner_paths_test;
mod runner_workspace_translation_test;
mod service_auth_test;
mod service_containers_test;
mod shell_templates_container_test;
mod shell_templates_replay_test;
mod shell_templates_test;
mod stable_workspace_test;
mod step_attrs_replay_test;
mod step_context_identity_test;
mod step_metadata_test;
mod step_process_ci_test;
