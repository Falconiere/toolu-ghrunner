//! Daemon configuration: settings sourced from the process environment, plus
//! the bearer token file, which is re-read fresh on every call so rotating
//! it takes effect without a daemon restart.

use std::env;
use std::fmt;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

/// Default bind address when `TOOLU_DAEMON_BIND` is unset.
const DEFAULT_BIND: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080));
/// Default queue depth ceiling when `TOOLU_DAEMON_QUEUE_MAX` is unset.
const DEFAULT_QUEUE_MAX: u32 = 32;
/// Default container runtime when `TOOLU_DAEMON_RUNTIME` is unset.
const DEFAULT_RUNTIME: &str = "sysbox-runc";

/// Errors produced while loading daemon configuration or reading the token file.
#[derive(Debug)]
pub enum ConfigError {
  /// A required environment variable was not set.
  MissingEnv(&'static str),
  /// An environment variable was set but not valid UTF-8.
  InvalidEnvEncoding(&'static str),
  /// An environment variable was set but failed to parse into the expected type.
  InvalidEnvValue {
    /// The environment variable name.
    var: &'static str,
    /// The raw value that failed to parse.
    value: String,
  },
  /// The token file could not be read (missing, unreadable, etc).
  TokenFileRead {
    /// Path to the token file.
    path: PathBuf,
    /// The underlying I/O error.
    source: std::io::Error,
  },
  /// The token file exists but is empty, or its first line is empty.
  TokenFileEmpty(PathBuf),
}

impl fmt::Display for ConfigError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::MissingEnv(var) => write!(f, "missing required environment variable {var}"),
      Self::InvalidEnvEncoding(var) => {
        write!(f, "environment variable {var} is not valid UTF-8")
      },
      Self::InvalidEnvValue { var, value } => {
        write!(
          f,
          "environment variable {var} has an invalid value: {value:?}"
        )
      },
      Self::TokenFileRead { path, source } => {
        write!(f, "failed to read token file {}: {source}", path.display())
      },
      Self::TokenFileEmpty(path) => write!(f, "token file {} is empty", path.display()),
    }
  }
}

impl std::error::Error for ConfigError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::MissingEnv(_)
      | Self::InvalidEnvEncoding(_)
      | Self::InvalidEnvValue { .. }
      | Self::TokenFileEmpty(_) => None,
      Self::TokenFileRead { source, .. } => Some(source),
    }
  }
}

/// Where configuration values are read from — the real process environment
/// in production, and an in-memory map in tests, so tests never mutate
/// global process state.
trait EnvSource {
  /// Look up a variable, returning `Ok(None)` when it is unset.
  fn lookup(&self, var: &'static str) -> Result<Option<String>, ConfigError>;
}

/// Reads from the real process environment.
struct ProcessEnv;

impl EnvSource for ProcessEnv {
  fn lookup(&self, var: &'static str) -> Result<Option<String>, ConfigError> {
    match env::var(var) {
      Ok(value) => Ok(Some(value)),
      Err(env::VarError::NotPresent) => Ok(None),
      Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidEnvEncoding(var)),
    }
  }
}

/// Read a required variable, erroring if it is unset.
fn required_env(reader: &impl EnvSource, var: &'static str) -> Result<String, ConfigError> {
  reader.lookup(var)?.ok_or(ConfigError::MissingEnv(var))
}

/// Read a required variable and parse it, erroring if it is unset or invalid.
fn parse_required<T>(reader: &impl EnvSource, var: &'static str) -> Result<T, ConfigError>
where
  T: std::str::FromStr,
{
  let raw = required_env(reader, var)?;
  raw
    .parse::<T>()
    .map_err(|_parse_err| ConfigError::InvalidEnvValue { var, value: raw })
}

/// Read an optional variable and parse it, falling back to `default` when unset.
fn parse_optional<T>(
  reader: &impl EnvSource,
  var: &'static str,
  default: T,
) -> Result<T, ConfigError>
where
  T: std::str::FromStr,
{
  match reader.lookup(var)? {
    Some(raw) => raw
      .parse::<T>()
      .map_err(|_parse_err| ConfigError::InvalidEnvValue { var, value: raw }),
    None => Ok(default),
  }
}

/// Daemon configuration sourced from the process environment at startup.
#[derive(Debug, Clone)]
pub struct Config {
  /// Path to the bearer token file, re-read fresh on every request.
  pub token_file: PathBuf,
  /// Address the HTTP server binds to.
  pub bind: SocketAddr,
  /// Whole vCPU budget available for concurrent jobs.
  pub vcpu: u32,
  /// Memory budget, in MB, available for concurrent jobs.
  pub memory_mb: u32,
  /// Maximum queue depth before a request is refused.
  pub queue_max: u32,
  /// Container runtime passed to Docker (e.g. `sysbox-runc`).
  pub runtime: String,
  /// OCI image reference pre-pulled at startup.
  pub image: String,
}

impl Config {
  /// Load configuration from the process environment.
  ///
  /// # Errors
  ///
  /// Returns [`ConfigError`] if a required variable is missing, a set
  /// variable is not valid UTF-8, or a value fails to parse.
  pub fn from_env() -> Result<Self, ConfigError> {
    Self::from_reader(&ProcessEnv)
  }

  /// Build a [`Config`] from any [`EnvSource`] — the real process
  /// environment in production, an in-memory map in tests.
  fn from_reader(reader: &impl EnvSource) -> Result<Self, ConfigError> {
    let token_file = PathBuf::from(required_env(reader, "TOOLU_DAEMON_TOKEN_FILE")?);
    let bind = parse_optional(reader, "TOOLU_DAEMON_BIND", DEFAULT_BIND)?;
    let vcpu = parse_required(reader, "TOOLU_DAEMON_VCPU")?;
    let memory_mb = parse_required(reader, "TOOLU_DAEMON_MEMORY_MB")?;
    let queue_max = parse_optional(reader, "TOOLU_DAEMON_QUEUE_MAX", DEFAULT_QUEUE_MAX)?;
    let runtime = parse_optional(reader, "TOOLU_DAEMON_RUNTIME", DEFAULT_RUNTIME.to_owned())?;
    let image = required_env(reader, "TOOLU_DAEMON_IMAGE")?;

    Ok(Self {
      token_file,
      bind,
      vcpu,
      memory_mb,
      queue_max,
      runtime,
      image,
    })
  }
}

/// The current and, if present, previous bearer token accepted during rotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
  /// The active bearer token.
  pub current: String,
  /// The token that was active before the most recent rotation, if any.
  pub previous: Option<String>,
}

/// Read the token file fresh — never cached — so rotating the file at the
/// configured path takes effect without a daemon restart.
///
/// Line 1 is the current token; an optional line 2 is the previous token,
/// accepted during rotation. Both are trimmed of surrounding whitespace.
///
/// # Errors
///
/// Returns [`ConfigError::TokenFileRead`] if the file is missing or cannot
/// be read, or [`ConfigError::TokenFileEmpty`] if the file or its first line
/// is empty.
pub fn read_tokens(path: &Path) -> Result<Tokens, ConfigError> {
  let contents = fs::read_to_string(path).map_err(|source| ConfigError::TokenFileRead {
    path: path.to_owned(),
    source,
  })?;

  let mut lines = contents.lines();
  let current = lines.next().unwrap_or("").trim();
  if current.is_empty() {
    return Err(ConfigError::TokenFileEmpty(path.to_owned()));
  }

  let previous = lines
    .next()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_owned);

  Ok(Tokens {
    current: current.to_owned(),
    previous,
  })
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
