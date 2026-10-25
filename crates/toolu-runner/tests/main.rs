//! The single `toolu-runner` integration-test binary (#108).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p toolu-runner --test integration -- register_labels_test::`.
//! `auth_store_test` and `secret_sink_coverage_test` mutate process-wide env
//! and the global tracing subscriber, so they live in the separate
//! `process_env` target (`tests/process_env.rs`).
//!
//! The shared helpers under `tests/helpers/` (and the `network` crate's
//! fixtures) are declared once here; test files reach them with
//! `use crate::<helper>;`. The `#[path]` attributes below are required: a bare
//! `mod x;` here would look for `tests/x.rs`, and `tests/` must stay flat, so
//! the helpers stay in `tests/helpers/`. `proxy_fixtures` / `tls_fixtures` are
//! included from `../../network/tests/helpers/`, shared with that crate's tests.

#[path = "helpers/boot_fixtures.rs"]
mod boot_fixtures;
#[path = "helpers/check_servers.rs"]
mod check_servers;
#[path = "helpers/doctor_env.rs"]
mod doctor_env;
#[path = "helpers/github_app_fixtures.rs"]
mod github_app_fixtures;
#[cfg(feature = "live")]
#[path = "helpers/live_harness.rs"]
mod harness;
#[path = "../../network/tests/helpers/proxy_fixtures.rs"]
mod proxy_fixtures;
#[path = "helpers/supervisor_fixtures.rs"]
mod supervisor_fixtures;
#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;
#[path = "helpers/toolu_fixtures.rs"]
mod toolu_fixtures;

mod actions_resolver_test;
mod annotation_live;
mod boot_cmd_e2e_test;
mod boot_cmd_test;
mod cache_accelerated_test;
mod cache_blob_test;
mod cache_fsync_wiring_test;
mod cache_l2_test;
mod cache_live_docker;
mod cache_live_e2e;
mod cache_poisoning_test;
mod cache_proxy_test;
mod cache_scope_test;
mod cache_server_test;
mod cache_twirp_test;
mod cache_v1_test;
mod cas_dedup_test;
mod cas_gc_test;
mod cas_index_test;
mod cas_manifest_test;
mod cas_store_test;
mod check_cmd_test;
mod check_tools_test;
mod cli_test;
mod composite_action_test;
mod composite_continue_on_error_test;
mod config_resolve_test;
mod create_app_test;
mod device_auth_test;
mod doctor_auth_test;
mod doctor_cmd_test;
mod doctor_flags_test;
mod doctor_network_test;
mod doctor_release_test;
mod doctor_service_test;
mod ephemeral_run_test;
mod execute_job_error_reporting_test;
mod expression_eval_test;
mod failure_modes_test;
mod gh_compat_actions;
mod gh_compat_cancel;
mod gh_compat_commands;
mod gh_compat_context;
mod gh_compat_deadlock;
mod gh_compat_decrypt;
mod gh_compat_fixture;
mod gh_compat_forwarder;
mod gh_compat_job_outputs;
mod gh_compat_live_log;
mod gh_compat_node_action_file_commands;
mod gh_compat_poll_cursor;
mod gh_compat_prepost;
mod gh_compat_register;
mod gh_compat_step_attrs;
mod gh_compat_streaming;
mod ghes_v1_test;
mod github_app_wire_test;
mod hash_files_test;
mod headless_service_test;
mod http2_features_test;
mod install_service_test;
mod job_container_live;
mod job_container_live_command;
mod job_teardown_order_test;
mod journal_reader_test;
mod journal_types_test;
mod journal_writer_test;
mod listener_smoke_test;
mod live_e2e;
mod live_login_e2e;
mod log_upload_mode_test;
mod machine_service_test;
mod net_test;
mod network_cli_test;
mod network_client_inventory_test;
mod network_journal_mask_test;
mod oidc_token_test;
mod poll_loop_test;
mod register_app_test;
mod register_labels_test;
mod register_replace_network_test;
mod register_zero_arg_test;
mod remove_unregister_test;
mod run_app_remint_test;
mod run_loop_live;
mod run_loop_warn_test;
mod run_service_classify_test;
mod secret_masker_init_test;
mod setup_cmd_test;
mod secret_masker_real_test;
mod session_conflict_e2e_test;
mod shadow_mode_test;
mod storage_layout_test;
mod supervisor_fatal_test;
mod supervisor_remove_test;
mod supervisor_test;
mod workspace_gc_test;
