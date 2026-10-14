//! The single `config` integration-test binary (#109).
//!
//! Every file in `tests/` is declared here as a module, so Cargo compiles and
//! links one executable instead of one per file (`autotests = false` in
//! `Cargo.toml`). Test names therefore carry the file stem as a module prefix:
//! `cargo test -p config --test integration -- app_store_test::`.

mod app_store_test;
mod auth_store_no_keyring_test;
mod home_perms_test;
mod labels_test;
mod lockfile_multi_test;
mod network_section_test;
mod registry_test;
mod remint_test;
mod repo_infer_test;
mod secret_file_perms_test;
mod service_action_test;
mod service_unit_test;
mod workspace_section_test;
