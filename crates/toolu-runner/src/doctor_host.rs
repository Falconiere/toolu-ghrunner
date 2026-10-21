//! `doctor` install and host rows: the build and compatibility versions, the
//! latest-release check, the platform, the effective uid and free disk.
//!
//! The release check is one unauthenticated GET of
//! `TOOLU_RUNNER_RELEASES_URL` (default: this repository's latest GitHub
//! release); any failure to check is a `SKIP`, never a problem.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::doctor_report::{Level, Row};

/// The one-liner that installs (or upgrades to) the latest release.
pub(crate) const INSTALL_ONE_LINER: &str =
  "curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash";

const GIB: u64 = 1024 * 1024 * 1024;

/// Below this much free space a job will almost certainly fail.
const DISK_FAIL_BELOW: u64 = GIB;

/// Below this much free space a checkout plus caches is at risk.
const DISK_WARN_BELOW: u64 = 5 * GIB;

/// The build's product version and the GitHub runner version it emulates.
pub(crate) fn version_row() -> Row {
  Row::new(
    Level::Ok,
    "version",
    format!(
      "toolu-runner {} (GitHub runner compatibility {})",
      env!("CARGO_PKG_VERSION"),
      protocol::runner_version::COMPATIBILITY_VERSION
    ),
  )
}

/// `MAJOR.MINOR.PATCH` with an optional leading `v`, as numbers. Anything
/// else — including a prerelease or build suffix — is `None`.
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
  let raw = raw.strip_prefix('v').unwrap_or(raw);
  let mut parts = raw.split('.');
  let major = parts.next()?.parse().ok()?;
  let minor = parts.next()?.parse().ok()?;
  let patch = parts.next()?.parse().ok()?;
  if parts.next().is_some() {
    return None;
  }
  Some((major, minor, patch))
}

/// How the `current` build version compares with the `latest_tag` release;
/// `None` when either is not a plain `MAJOR.MINOR.PATCH`.
pub(crate) fn compare_release(current: &str, latest_tag: &str) -> Option<Ordering> {
  Some(parse_version(current)?.cmp(&parse_version(latest_tag)?))
}

/// The `update` row for a known latest release tag.
pub(crate) fn update_row(current: &str, latest_tag: &str) -> Row {
  match compare_release(current, latest_tag) {
    Some(Ordering::Equal) => Row::new(
      Level::Ok,
      "update",
      format!("up to date (latest {latest_tag})"),
    ),
    Some(Ordering::Greater) => Row::new(
      Level::Ok,
      "update",
      format!("newer than the latest release ({latest_tag})"),
    ),
    Some(Ordering::Less) => Row::new(
      Level::Warn,
      "update",
      format!("{latest_tag} is available (installed {current})"),
    )
    .with_fix(INSTALL_ONE_LINER),
    None => Row::new(
      Level::Skip,
      "update",
      format!("cannot compare {current} with the latest release {latest_tag}"),
    ),
  }
}

/// The `update` row through `network`: `SKIP` offline or when the check
/// itself fails.
pub(crate) async fn update_check(
  offline: bool,
  timeout: Duration,
  network: &shared::NetworkConfig,
) -> Row {
  if offline {
    return Row::new(Level::Skip, "update", "offline");
  }
  match latest_tag(&crate::config::releases_url(), timeout, network).await {
    Ok(tag) => update_row(env!("CARGO_PKG_VERSION"), &tag),
    Err(reason) => Row::new(
      Level::Skip,
      "update",
      format!("could not check latest release: {reason}"),
    ),
  }
}

/// GET the release document and read its `tag_name`.
async fn latest_tag(
  url: &str,
  timeout: Duration,
  network: &shared::NetworkConfig,
) -> Result<String, String> {
  let client = crate::check_probe::build_client(timeout, network)?;
  let response = client
    .get(url)
    .header("Accept", "application/vnd.github+json")
    .send()
    .await
    // `without_url`: the mirror URL's query can carry a token.
    .map_err(|error| crate::check_probe::describe_error(&error.without_url()))?;
  let status = response.status();
  if !status.is_success() {
    return Err(format!("HTTP {}", status.as_u16()));
  }
  let body: serde_json::Value = response
    .json()
    .await
    .map_err(|error| format!("unreadable response: {}", error.without_url()))?;
  body
    .get("tag_name")
    .and_then(serde_json::Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| "the response has no tag_name".to_owned())
}

/// The OS and CPU in GitHub's `RUNNER_OS` / `RUNNER_ARCH` spelling.
pub(crate) fn platform_row() -> Row {
  Row::new(
    Level::Ok,
    "platform",
    format!(
      "{} {}",
      shared::platform::runner_os(),
      shared::platform::runner_arch()
    ),
  )
}

/// The `user` row for a uid string: root is a warning, a non-number unknown.
pub(crate) fn uid_row(uid: &str) -> Row {
  if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
    return Row::new(
      Level::Warn,
      "user",
      format!("could not determine the uid ({uid:?})"),
    );
  }
  if uid == "0" {
    return Row::new(
      Level::Warn,
      "user",
      "running as root (uid 0): every job step would run as root",
    )
    .with_fix("run the runner as a dedicated unprivileged user");
  }
  Row::new(Level::Ok, "user", format!("uid {uid}"))
}

/// The `user` row for this process (`id -u`).
pub(crate) fn user_row() -> Row {
  match crate::service_cmd::current_uid() {
    Ok(uid) => uid_row(&uid),
    Err(error) => Row::new(
      Level::Warn,
      "user",
      format!("could not determine the uid: {error}"),
    ),
  }
}

/// `FAIL` below 1 GiB, `WARN` below 5 GiB, else `OK`.
pub(crate) fn disk_level(free_bytes: u64) -> Level {
  if free_bytes < DISK_FAIL_BELOW {
    Level::Fail
  } else if free_bytes < DISK_WARN_BELOW {
    Level::Warn
  } else {
    Level::Ok
  }
}

/// `path` itself when it exists, else its nearest existing ancestor — a work
/// dir that was never created lives on the volume of its parent.
pub(crate) fn nearest_existing(path: &Path) -> PathBuf {
  path
    .ancestors()
    .find(|ancestor| ancestor.exists())
    .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// One `disk` row per distinct path, measured on its nearest existing dir.
pub(crate) fn disk_rows(paths: &[PathBuf]) -> Vec<Row> {
  let mut seen: Vec<&PathBuf> = Vec::new();
  let mut rows = Vec::new();
  for path in paths {
    if seen.contains(&path) {
      continue;
    }
    seen.push(path);
    rows.push(disk_row(path, fs2::available_space(nearest_existing(path))));
  }
  rows
}

/// The `disk` row for `path` given its measured free bytes (or why they
/// could not be measured).
pub(crate) fn disk_row(path: &Path, free: std::io::Result<u64>) -> Row {
  match free {
    Ok(free) => {
      let level = disk_level(free);
      let row = Row::new(
        level,
        "disk",
        format!(
          "{:.1} GiB free at {}",
          free as f64 / GIB as f64,
          path.display()
        ),
      );
      if level == Level::Ok {
        row
      } else {
        row.with_fix(format!(
          "free space on the volume holding {}",
          path.display()
        ))
      }
    },
    Err(error) => Row::new(
      Level::Warn,
      "disk",
      format!(
        "could not measure free space at {}: {error}",
        path.display()
      ),
    ),
  }
}

#[cfg(test)]
#[path = "tests/doctor_host.rs"]
mod tests;
