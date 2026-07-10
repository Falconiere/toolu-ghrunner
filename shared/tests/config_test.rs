use shared::{RunnerConfig, ServicesMode};
use std::path::PathBuf;

#[test]
fn runner_config_default_values() {
  let cfg = RunnerConfig::default();
  assert_eq!(cfg.data_dir, PathBuf::new());
  assert_eq!(cfg.workspace_root, PathBuf::new());
  assert!(cfg.cgroup_path.is_none());
  // The default serving mode is the forwarder (real GitHub services).
  assert_eq!(cfg.services_mode, ServicesMode::Forwarder);
  // Non-loopback so docker-container BuildKit reaches the cache server.
  assert_eq!(cfg.service_bind, "0.0.0.0");
  assert_eq!(cfg.workspace_gc_hours, 24);
  assert!(!cfg.shadow_enabled);
  // CacheConfig defaults: 100 GiB L1, GitHub-matching 7-day TTL, 64 KiB
  // FastCDC target, no S3 cold tier.
  assert_eq!(cfg.cache.max_bytes, 100 * 1024 * 1024 * 1024);
  assert_eq!(cfg.cache.entry_ttl_days, 7);
  assert_eq!(cfg.cache.protected_branches, ["main", "master"]);
  assert_eq!(cfg.cache.chunk_avg_bytes, 64 * 1024);
  assert!(cfg.cache.l2.is_none());
}

#[test]
fn runner_config_holds_paths() {
  let cfg = RunnerConfig {
    data_dir: PathBuf::from("/var/lib/toolu-runner"),
    workspace_root: PathBuf::from("/var/lib/toolu-runner/_work"),
    cgroup_path: None,
    services_mode: ServicesMode::default(),
    ..RunnerConfig::default()
  };
  assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/toolu-runner"));
  assert_eq!(
    cfg.workspace_root,
    PathBuf::from("/var/lib/toolu-runner/_work")
  );
  assert!(cfg.cgroup_path.is_none());
  // The default serving mode is the forwarder (real GitHub services).
  assert_eq!(cfg.services_mode, ServicesMode::Forwarder);
}

#[test]
fn runner_config_with_cgroup() {
  let cfg = RunnerConfig {
    data_dir: PathBuf::from("/var/lib/toolu-runner"),
    workspace_root: PathBuf::from("/var/lib/toolu-runner/_work"),
    cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/toolu-runner/job-123")),
    services_mode: ServicesMode::default(),
    ..RunnerConfig::default()
  };
  assert_eq!(
    cfg.cgroup_path.as_deref(),
    Some(std::path::Path::new("/sys/fs/cgroup/toolu-runner/job-123"))
  );
}

#[test]
fn runner_config_clone() {
  let cfg = RunnerConfig {
    data_dir: PathBuf::from("/a"),
    workspace_root: PathBuf::from("/b"),
    cgroup_path: Some(PathBuf::from("/c")),
    services_mode: ServicesMode::Offline,
    ..RunnerConfig::default()
  };
  let clone = cfg.clone();
  assert_eq!(clone.data_dir, cfg.data_dir);
  assert_eq!(clone.workspace_root, cfg.workspace_root);
  assert_eq!(clone.cgroup_path, cfg.cgroup_path);
}
