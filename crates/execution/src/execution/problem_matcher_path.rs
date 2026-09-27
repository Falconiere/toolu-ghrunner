//! Bounded matcher-file reads and upstream checkout-relative diagnostic paths.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::context::ExecutionContext;

const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Read a regular matcher file, rooting command paths at the job workspace.
pub(super) fn read_matcher(path: &str, ctx: &ExecutionContext) -> Result<String, String> {
  let path = command_path(path, ctx);
  read_regular(&path)
    .map_err(|error| format!("unable to load problem matcher {}: {error}", path.display()))
}

fn command_path(path: &str, ctx: &ExecutionContext) -> PathBuf {
  let path = Path::new(path);
  let path = ctx.job_container().map_or_else(
    || path.to_path_buf(),
    |container| container.translator().to_host(path),
  );
  workspace(ctx).join(path)
}

fn workspace(ctx: &ExecutionContext) -> PathBuf {
  ctx.workspace.as_deref().map_or_else(
    || PathBuf::from(ctx.github_context("workspace").unwrap_or(".")),
    Path::to_path_buf,
  )
}

fn read_regular(path: &Path) -> Result<String, String> {
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  // Opening a FIFO must never wedge command processing, even after a path swap.
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(nix::libc::O_NONBLOCK);
  }
  let file = options.open(path).map_err(|error| error.to_string())?;
  let metadata = file.metadata().map_err(|error| error.to_string())?;
  if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
    return Err("expected a regular file of at most 1 MiB".to_owned());
  }
  let mut text = String::new();
  file
    .take(MAX_FILE_BYTES + 1)
    .read_to_string(&mut text)
    .map_err(|error| error.to_string())?;
  if u64::try_from(text.len()).map_or(true, |size| size > MAX_FILE_BYTES) {
    return Err("file exceeds 1 MiB".to_owned());
  }
  Ok(text)
}

/// Resolve a matched path only when the file belongs to the workflow checkout.
pub(super) fn annotation_path(
  file: Option<&str>,
  from_path: Option<&str>,
  ctx: &ExecutionContext,
) -> Option<String> {
  let file = file.filter(|file| !file.trim().is_empty())?;
  let mut path = PathBuf::from(file);
  if path.is_relative()
    && let Some(parent) = from_path
      .filter(|value| !value.trim().is_empty())
      .and_then(|value| Path::new(value).parent())
  {
    path = parent.join(path);
  }
  if path.is_relative() {
    path = workspace(ctx).join(path);
  } else if let Some(container) = ctx.job_container() {
    path = container.translator().to_host(&path);
  }
  let path = normalize(&path);
  if !path.is_file() {
    return None;
  }
  let repository = ctx.github_context("repository")?;
  let server = ctx
    .github_context("server_url")
    .filter(|value| !value.is_empty())
    .unwrap_or("https://github.com");
  let host = reqwest::Url::parse(server).ok()?.host_str()?.to_owned();
  let https = format!("url = {server}/{repository}");
  let ssh = format!("url = git@{host}:{repository}.git");
  for directory in path.ancestors().skip(1).take(51) {
    let config = directory.join(".git/config");
    if !config.is_file() {
      continue;
    }
    let text = read_regular(&config).ok()?;
    if text.lines().any(|line| {
      line.trim().eq_ignore_ascii_case(&https) || line.trim().eq_ignore_ascii_case(&ssh)
    }) {
      return path.strip_prefix(directory).ok().map(|path| {
        path
          .to_string_lossy()
          .replace(std::path::MAIN_SEPARATOR, "/")
      });
    }
    // A nested repository for a different origin is not the workflow checkout.
    return None;
  }
  None
}

fn normalize(path: &Path) -> PathBuf {
  let mut output = PathBuf::new();
  for part in path.components() {
    match part {
      Component::CurDir => {},
      Component::ParentDir => {
        output.pop();
      },
      part @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
        output.push(part.as_os_str());
      },
    }
  }
  output
}
