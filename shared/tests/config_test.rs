use shared::RunnerConfig;
use std::path::PathBuf;

#[test]
fn runner_config_holds_paths() {
  let cfg = RunnerConfig {
    data_dir: PathBuf::from("/var/lib/toolu-runner"),
    workspace_root: PathBuf::from("/var/lib/toolu-runner/_work"),
    cgroup_path: None,
  };
  assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/toolu-runner"));
  assert_eq!(
    cfg.workspace_root,
    PathBuf::from("/var/lib/toolu-runner/_work")
  );
  assert!(cfg.cgroup_path.is_none());
}

#[test]
fn runner_config_with_cgroup() {
  let cfg = RunnerConfig {
    data_dir: PathBuf::from("/var/lib/toolu-runner"),
    workspace_root: PathBuf::from("/var/lib/toolu-runner/_work"),
    cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/toolu-runner/job-123")),
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
  };
  let clone = cfg.clone();
  assert_eq!(clone.data_dir, cfg.data_dir);
  assert_eq!(clone.workspace_root, cfg.workspace_root);
  assert_eq!(clone.cgroup_path, cfg.cgroup_path);
}
