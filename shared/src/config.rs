use std::path::PathBuf;

/// How the runner serves artifacts / cache / OIDC to step actions.
///
/// - [`Forwarder`](ServicesMode::Forwarder) (default) copies the real
///   GitHub service URLs + runtime token out of the job message into step
///   env, so the JS toolkit `@v4` actions talk to real GitHub — matching the
///   official `actions/runner`.
/// - [`Offline`](ServicesMode::Offline) hosts local fake artifact/cache/OIDC
///   services and points step env at them, for hermetic / airgapped runs.
/// - [`Accelerated`](ServicesMode::Accelerated) forwards everything except
///   cache traffic, which a local content-addressed store serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServicesMode {
  /// Forward real GitHub service URLs from the job message (default).
  #[default]
  Forwarder,
  /// Host local services and wire step env at them.
  Offline,
  /// Forward everything but cache; a local CAS intercepts cache traffic.
  Accelerated,
}

/// Optional S3 cold tier that mirrors immutable chunks + manifests (never the
/// index). Absent means L1-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L2Config {
  /// S3 bucket the chunk/manifest mirror lives in.
  pub bucket: String,
  /// S3-compatible endpoint URL.
  pub endpoint: String,
  /// S3 region.
  pub region: String,
}

/// Content-addressed cache settings (`[cache]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
  /// L1 NVMe eviction ceiling in bytes.
  pub max_bytes: u64,
  /// Entry TTL in days (matches GitHub's 7).
  pub entry_ttl_days: u64,
  /// Branches a `Trusted` job may write the shared scope for.
  pub protected_branches: Vec<String>,
  /// FastCDC target average chunk size in bytes.
  pub chunk_avg_bytes: u32,
  /// S3 cold tier, or `None` for L1-only.
  pub l2: Option<L2Config>,
}

impl Default for CacheConfig {
  fn default() -> Self {
    Self {
      max_bytes: 100 * 1024 * 1024 * 1024,
      entry_ttl_days: 7,
      protected_branches: vec!["main".to_owned(), "master".to_owned()],
      chunk_avg_bytes: 64 * 1024,
      l2: None,
    }
  }
}

/// Runner environment configuration — no job-specific data.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
  /// Root directory for caches (actions, node, tools).
  pub data_dir: PathBuf,
  /// Parent directory for per-job workspace subdirectories.
  pub workspace_root: PathBuf,
  /// Per-job cgroup-v2 directory used to enforce CPU/memory limits.
  ///
  /// `None` in v1 (no cgroup isolation — runners run in the user's session).
  /// Reserved for a future v1.1 capability.
  pub cgroup_path: Option<PathBuf>,
  /// Artifact/cache/OIDC serving mode (`forwarder` default).
  pub services_mode: ServicesMode,
  /// Address the accelerated cache server binds. Must not be loopback:
  /// `docker-container` BuildKit reaches it across a network namespace.
  pub service_bind: String,
  /// Content-addressed cache settings (accelerated mode).
  pub cache: CacheConfig,
  /// Age in hours after which a finished job's workspace is pruned.
  pub workspace_gc_hours: u64,
  /// Whether shadow-mode step observation records (never serves).
  pub shadow_enabled: bool,
}

impl Default for RunnerConfig {
  fn default() -> Self {
    Self {
      data_dir: PathBuf::new(),
      workspace_root: PathBuf::new(),
      cgroup_path: None,
      services_mode: ServicesMode::default(),
      service_bind: "0.0.0.0".to_owned(),
      cache: CacheConfig::default(),
      workspace_gc_hours: 24,
      shadow_enabled: false,
    }
  }
}
