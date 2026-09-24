//! Tests for typed Docker API create-option translation.

use bollard::models::ContainerCreateBody;

use super::apply_container_options;

/// Fractional CPU workflow syntax maps exactly to Docker's integer nano-CPU
/// field, avoiding a lossy float conversion at the API boundary.
#[test]
fn maps_fractional_cpus_to_nano_cpus() {
  let mut body = ContainerCreateBody::default();
  let options = vec!["--cpus".to_owned(), "1.25".to_owned()];
  let result = apply_container_options(&options, &mut body);

  assert!(result.is_ok(), "fractional CPUs must map: {result:?}");
  assert!(body.host_config.is_some());
  if let Some(config) = body.host_config {
    assert_eq!(config.nano_cpus, Some(1_250_000_000));
  }
}

/// Docker's binary shorthand and optional trailing-byte suffixes preserve the
/// values users supply in real `container.options` workflow declarations.
#[test]
fn maps_memory_suffixes_and_rejects_invalid_values() {
  let mut body = ContainerCreateBody::default();
  let options = vec![
    "--memory".to_owned(),
    "512m".to_owned(),
    "--memory-swap".to_owned(),
    "2gb".to_owned(),
  ];
  let result = apply_container_options(&options, &mut body);

  assert!(result.is_ok(), "memory options must map: {result:?}");
  assert!(body.host_config.is_some());
  if let Some(config) = body.host_config {
    assert_eq!(config.memory, Some(536_870_912));
    assert_eq!(config.memory_swap, Some(2_147_483_648));
  }

  let mut invalid = ContainerCreateBody::default();
  let invalid_options = vec!["--memory".to_owned(), "1.5m".to_owned()];
  assert!(apply_container_options(&invalid_options, &mut invalid).is_err());
}

/// The supported identity, resource, capability, security, and ulimit values
/// all arrive in the corresponding typed Docker request fields.
#[test]
fn maps_declared_option_families_to_docker_create_fields() {
  let mut body = ContainerCreateBody::default();
  let options = vec![
    "--user".to_owned(),
    "1001:1001".to_owned(),
    "--hostname".to_owned(),
    "build".to_owned(),
    "--domainname".to_owned(),
    "example.test".to_owned(),
    "--cpu-shares".to_owned(),
    "1024".to_owned(),
    "--pids-limit".to_owned(),
    "128".to_owned(),
    "--group-add".to_owned(),
    "wheel".to_owned(),
    "--cap-add".to_owned(),
    "NET_ADMIN".to_owned(),
    "--cap-drop".to_owned(),
    "MKNOD".to_owned(),
    "--security-opt".to_owned(),
    "no-new-privileges:true".to_owned(),
    "--ulimit".to_owned(),
    "nofile=1024:2048".to_owned(),
    "--privileged".to_owned(),
    "--read-only".to_owned(),
  ];
  let result = apply_container_options(&options, &mut body);

  assert!(result.is_ok(), "declared options must map: {result:?}");
  assert_eq!(body.user.as_deref(), Some("1001:1001"));
  assert_eq!(body.hostname.as_deref(), Some("build"));
  assert_eq!(body.domainname.as_deref(), Some("example.test"));
  assert!(body.host_config.is_some());
  if let Some(config) = body.host_config {
    assert_eq!(config.cpu_shares, Some(1024));
    assert_eq!(config.pids_limit, Some(128));
    assert_eq!(config.group_add, Some(vec!["wheel".to_owned()]));
    assert_eq!(config.cap_add, Some(vec!["NET_ADMIN".to_owned()]));
    assert_eq!(config.cap_drop, Some(vec!["MKNOD".to_owned()]));
    assert_eq!(
      config.security_opt,
      Some(vec!["no-new-privileges:true".to_owned()])
    );
    assert_eq!(config.privileged, Some(true));
    assert_eq!(config.readonly_rootfs, Some(true));
    assert_eq!(config.ulimits.as_ref().map(Vec::len), Some(1));
  }
}
