use std::path::PathBuf;

/// How the runner serves artifacts / cache / OIDC to step actions.
///
/// - [`Forwarder`](ServicesMode::Forwarder) (default) copies the real
///   GitHub service URLs + runtime token out of the job message into step
///   env, so the JS toolkit `@v4` actions talk to real GitHub — matching the
///   official `actions/runner`.
/// - [`Offline`](ServicesMode::Offline) starts only a local legacy v1 cache
///   and points `ACTIONS_CACHE_URL` / `ACTIONS_RUNTIME_TOKEN` at it; no
///   artifact/OIDC service is started and no GitHub URL is forwarded.
/// - [`Accelerated`](ServicesMode::Accelerated) forwards everything except
///   cache traffic, which a local content-addressed store serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServicesMode {
  /// Forward real GitHub service URLs from the job message (default).
  #[default]
  Forwarder,
  /// Start only the local v1 cache and wire step env at it.
  Offline,
  /// Forward everything but cache; a local CAS intercepts cache traffic.
  Accelerated,
}

/// How a job's workspace directory is allocated (`[workspace] mode`).
///
/// - [`Stable`](WorkspaceMode::Stable) (default) maps each repository to
///   `<work>/<repo>/<repo>` and reuses it across jobs, like the official
///   runner's `_work/<repo>/<repo>`, so `actions/checkout` fetches
///   incrementally.
/// - [`Fresh`](WorkspaceMode::Fresh) creates a new `<work>/<job_id>` for
///   every job (the pre-#90 layout).
///
/// The default is [`Stable`](WorkspaceMode::Stable), so a config without
/// `[workspace] mode` (and [`WorkspaceConfig::default`]) reuses workspaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceMode {
  /// Reuse one workspace per repository (default; upstream behavior).
  #[default]
  Stable,
  /// A new workspace for every job.
  Fresh,
}

/// Workspace layout and stable-workspace GC settings (`[workspace]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkspaceConfig {
  /// Stable per-repository or fresh per-job workspaces.
  pub mode: WorkspaceMode,
  /// Hours since a stable workspace's last job after which GC may prune it.
  /// `0` (default) never prunes stable workspaces, as upstream.
  pub stable_gc_hours: u64,
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
  /// L1 `NVMe` eviction ceiling in bytes.
  pub max_bytes: u64,
  /// Entry TTL in days (matches GitHub's 7).
  pub entry_ttl_days: u64,
  /// Branches a `Trusted` job may write the shared scope for.
  pub protected_branches: Vec<String>,
  /// `FastCDC` target average chunk size in bytes.
  pub chunk_avg_bytes: u32,
  /// S3 cold tier, or `None` for L1-only.
  pub l2: Option<L2Config>,
  /// Restore the previous per-chunk `fsync` before rename. Default `false`:
  /// chunk integrity is guaranteed by BLAKE3 verify-on-read regardless, so a
  /// torn chunk after an unclean shutdown is never served, self-heals on the
  /// next read, and degrades to a cache miss (or an L2 restore).
  pub fsync_chunks: bool,
}

impl CacheConfig {
  /// Default cache capacity: 100 GiB. Backs the `[cache] max_bytes` TOML
  /// default in `toolu-runner::config` (`default_cache_max_bytes`) so the two
  /// sites cannot drift.
  pub const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;
  /// Default entry TTL: 7 days (matches GitHub). Backs `default_entry_ttl_days`.
  pub const DEFAULT_ENTRY_TTL_DAYS: u64 = 7;
  /// Default `FastCDC` target average chunk size: 64 KiB. Backs
  /// `default_chunk_avg_bytes`.
  pub const DEFAULT_CHUNK_AVG_BYTES: u32 = 64 * 1024;
  /// Default protected branches a `Trusted` job may write the shared scope for.
  /// Backs `default_protected_branches`.
  pub const DEFAULT_PROTECTED_BRANCHES: &'static [&'static str] = &["main", "master"];
}

impl Default for CacheConfig {
  fn default() -> Self {
    Self {
      max_bytes: Self::DEFAULT_MAX_BYTES,
      entry_ttl_days: Self::DEFAULT_ENTRY_TTL_DAYS,
      protected_branches: Self::DEFAULT_PROTECTED_BRANCHES
        .iter()
        .map(|b| (*b).to_owned())
        .collect(),
      chunk_avg_bytes: Self::DEFAULT_CHUNK_AVG_BYTES,
      l2: None,
      fsync_chunks: false,
    }
  }
}

/// Explicit proxy and extra-CA settings (`[network]`, issue #94).
///
/// [`Default`] (every field empty) is *env mode*: runner HTTP clients keep
/// reqwest's system proxy (`HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` /
/// `NO_PROXY`). A non-empty [`proxy`](Self::proxy) is *config mode* and
/// replaces those variables. [`ca_bundle`](Self::ca_bundle) adds trust anchors
/// in either mode. The `network` crate turns this into HTTP clients and step
/// env; this type is pure data so `protocol` (which depends on `shared`)
/// stays free of `reqwest`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct NetworkConfig {
  /// `http://` or `https://` proxy for http and https destinations. May carry
  /// `user:password@` credentials — never print it; [`Debug`] shows only `<set>`.
  pub proxy: Option<String>,
  /// Bypass entries in `NO_PROXY` syntax, used only with [`proxy`](Self::proxy).
  pub no_proxy: Vec<String>,
  /// Absolute path to a PEM bundle of extra trusted CA certificates.
  pub ca_bundle: Option<PathBuf>,
}

impl std::fmt::Debug for NetworkConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NetworkConfig")
      .field(
        "proxy",
        // Never the value: it may carry credentials, and parsing proxy URLs
        // belongs to the `network` crate (its `redact_url`).
        &self.proxy.as_ref().map(|_| "<set>"),
      )
      .field("no_proxy", &self.no_proxy)
      .field("ca_bundle", &self.ca_bundle)
      .finish()
  }
}

/// Runner environment configuration — no job-specific data.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
  /// Root directory for caches (actions, node, tools).
  pub data_dir: PathBuf,
  /// Work root holding job workspaces (`<repo>/<repo>` stable or
  /// `<job_id>` fresh) and `_PipelineMapping/`.
  pub workspace_root: PathBuf,
  /// Per-job cgroup-v2 directory used to enforce CPU/memory limits.
  ///
  /// `None` in v1 (no cgroup isolation — runners run in the user's session).
  /// Reserved for a future v1.1 capability.
  pub cgroup_path: Option<PathBuf>,
  /// Artifact/cache/OIDC serving mode.
  ///
  /// Defaults to [`ServicesMode::Forwarder`] (real GitHub service URLs
  /// forwarded from the job message) — both here and in
  /// [`ServicesMode::default`]. See [`ServicesMode`] for the `offline`
  /// and `accelerated` alternatives.
  pub services_mode: ServicesMode,
  /// Address the accelerated cache server binds. Must not be loopback:
  /// `docker-container` `BuildKit` reaches it across a network namespace.
  pub service_bind: String,
  /// Content-addressed cache settings (accelerated mode).
  pub cache: CacheConfig,
  /// Age in hours after which a finished fresh per-job workspace is pruned.
  /// `0` disables GC (never prune); a value at or above the 100-year cap also
  /// means never prune.
  pub workspace_gc_hours: u64,
  /// Workspace layout mode and stable-workspace GC age.
  pub workspace: WorkspaceConfig,
  /// Whether shadow-mode step observation records (never serves).
  pub shadow_enabled: bool,
  /// Offline Node.js runtimes in upstream's `externals/` layout
  /// (`node24/bin/node`, `node24_alpine/bin/node`), from
  /// `TOOLU_RUNNER_NODE_DIR`. When set it is authoritative: a missing
  /// runtime fails the step instead of downloading.
  pub node_dir: Option<PathBuf>,
  /// Explicit proxy / extra-CA settings; default is env mode.
  pub network: NetworkConfig,
}

/// Return `true` iff `TOOLU_RUNNER_ALLOW_VERBOSE` is exactly `"1"`.
///
/// Unset and all other Unicode values return `false`. Invalid Unicode is
/// reported directly to stderr because this read happens before tracing is
/// initialized, then also returns `false`.
pub fn allow_verbose() -> bool {
  match std::env::var("TOOLU_RUNNER_ALLOW_VERBOSE") {
    Ok(value) => value == "1",
    Err(std::env::VarError::NotPresent) => false,
    Err(std::env::VarError::NotUnicode(_)) => {
      eprintln!(
        "toolu-runner: TOOLU_RUNNER_ALLOW_VERBOSE is not valid Unicode; verbose logging remains disabled"
      );
      false
    },
  }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

impl RunnerConfig {
  /// Default [`service_bind`](Self::service_bind) address. Non-loopback so
  /// `docker-container` `BuildKit` reaches the cache server across a netns.
  /// Also backs the `[services] bind` TOML default in `toolu-runner::config`
  /// (`default_service_bind`) so the two sites cannot drift.
  pub const DEFAULT_SERVICE_BIND: &'static str = "0.0.0.0";
  /// Default workspace GC age: 24 hours. Backs the `[workspace] gc_hours` TOML
  /// default in `toolu-runner::config` (`default_gc_after_hours`) so the two
  /// sites cannot drift.
  pub const DEFAULT_WORKSPACE_GC_HOURS: u64 = 24;
}

impl Default for RunnerConfig {
  fn default() -> Self {
    Self {
      data_dir: PathBuf::new(),
      workspace_root: PathBuf::new(),
      cgroup_path: None,
      services_mode: ServicesMode::default(),
      service_bind: Self::DEFAULT_SERVICE_BIND.to_owned(),
      cache: CacheConfig::default(),
      workspace_gc_hours: Self::DEFAULT_WORKSPACE_GC_HOURS,
      workspace: WorkspaceConfig::default(),
      shadow_enabled: false,
      node_dir: None,
      network: NetworkConfig::default(),
    }
  }
}
