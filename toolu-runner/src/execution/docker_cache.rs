//! Docker layer caching contract.
//!
//! Defines the interface and types for persistent Docker layer caching
//! across jobs. Implementation-agnostic — the actual mechanism (BuildKit,
//! registry, or proxy) is determined at deployment.

/// Docker layer cache configuration per org.
pub struct DockerCacheConfig {
  /// Organization ID (cache scoped per org).
  pub org_id: String,
  /// Repository full name (cache further scoped per repo).
  pub repo: String,
  /// Maximum cache size in bytes (default 50 GB).
  pub quota_bytes: u64,
  /// Retention days before unused layers are evicted (default 14).
  pub retention_days: u32,
}

impl Default for DockerCacheConfig {
  fn default() -> Self {
    Self {
      org_id: String::new(),
      repo: String::new(),
      quota_bytes: 50 * 1024 * 1024 * 1024, // 50 GB
      retention_days: 14,
    }
  }
}

/// Platform identifier for multi-platform layer isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockerPlatform {
  LinuxAmd64,
  LinuxArm64,
}

impl DockerPlatform {
  pub fn as_str(&self) -> &'static str {
    match self {
      Self::LinuxAmd64 => "linux/amd64",
      Self::LinuxArm64 => "linux/arm64",
    }
  }
}

/// Cache key for Docker layers (org + repo + platform).
pub fn docker_layer_key(org_id: &str, repo: &str, platform: DockerPlatform) -> String {
  format!("docker/{org_id}/{repo}/{}", platform.as_str())
}

/// Event emitted when Docker layer cache is used.
#[derive(Debug, Clone)]
pub struct DockerCacheEvent {
  pub org_id: String,
  pub repo: String,
  pub platform: DockerPlatform,
  pub hit: bool,
  pub layers_cached: u32,
  pub layers_rebuilt: u32,
}
