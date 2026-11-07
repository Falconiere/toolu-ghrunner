//! Private disk spool for masked step and combined job logs.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use tokio::io::{AsyncWriteExt, BufWriter};

/// A private, line-ordered spool. Callers mask each line before appending it.
pub(crate) struct LogSpool {
  path: PathBuf,
  writer: Option<BufWriter<tokio::fs::File>>,
  line_count: u64,
}

impl LogSpool {
  /// Start the combined log for a job, removing files left by an interrupted run.
  pub(crate) fn for_job(data_dir: &Path, job_id: &str) -> io::Result<Self> {
    let dir = private_logs_dir(data_dir)?;
    let name = shared::paths::sanitize_job_id(job_id);
    let path = dir.join(format!("{name}.log"));
    remove_if_present(&path)?;
    remove_if_present(&path.with_extension("log.gz"))?;
    Self::new(path)
  }

  /// Start one step log in the same private directory.
  pub(crate) fn for_step(data_dir: &Path) -> io::Result<Self> {
    let dir = private_logs_dir(data_dir)?;
    Self::new(dir.join(format!("step-{}.log", uuid::Uuid::new_v4())))
  }

  fn new(path: PathBuf) -> io::Result<Self> {
    let writer = BufWriter::new(tokio::fs::File::from_std(create_private(&path)?));
    Ok(Self {
      path,
      writer: Some(writer),
      line_count: 0,
    })
  }

  /// Append one already-masked line, preserving its bytes and position.
  pub(crate) async fn append(&mut self, line: &str) -> io::Result<()> {
    let writer = self
      .writer
      .as_mut()
      .ok_or_else(|| io::Error::other("log spool closed"))?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    self.line_count += 1;
    Ok(())
  }

  /// Flush and hand the file to the upload task.
  pub(crate) async fn finish(mut self) -> io::Result<(PathBuf, u64)> {
    if let Some(mut writer) = self.writer.take() {
      writer.flush().await?;
    }
    let path = std::mem::take(&mut self.path);
    Ok((path, self.line_count))
  }
}

impl Drop for LogSpool {
  fn drop(&mut self) {
    if !self.path.as_os_str().is_empty() {
      self.writer.take();
      if let Err(error) = remove_if_present(&self.path) {
        tracing::warn!(error = %error, path = %self.path.display(), "could not remove log spool");
      }
    }
  }
}

fn private_logs_dir(data_dir: &Path) -> io::Result<PathBuf> {
  let dir = data_dir.join("_logs");
  std::fs::create_dir_all(data_dir)?;
  match std::fs::symlink_metadata(&dir) {
    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
      return Err(io::Error::other("log spool path is not a directory"));
    },
    Ok(_) => {},
    Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(&dir)?,
    Err(error) => return Err(error),
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(dir)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
  match std::fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

/// Create a new private file without following a pre-existing symlink.
pub(crate) fn create_private(path: &Path) -> io::Result<File> {
  let mut options = OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  options.open(path)
}

/// Compress a spool into a second private file without buffering it in memory.
pub(crate) fn gzip_file(path: &Path) -> io::Result<PathBuf> {
  let mut name = path.as_os_str().to_os_string();
  name.push(".gz");
  let gz_path = PathBuf::from(name);
  let result = (|| {
    let mut source = BufReader::new(File::open(path)?);
    let target = create_private(&gz_path)?;
    let mut encoder = GzEncoder::new(target, Compression::fast());
    io::copy(&mut source, &mut encoder)?;
    encoder.finish()?;
    Ok::<(), io::Error>(())
  })();
  if let Err(error) = result {
    if let Err(cleanup_error) = std::fs::remove_file(&gz_path)
      && cleanup_error.kind() != io::ErrorKind::NotFound
    {
      tracing::warn!(error = %cleanup_error, "could not remove incomplete compressed log");
    }
    return Err(error);
  }
  Ok(gz_path)
}
