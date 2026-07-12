//! FastCDC v2020 chunking of an assembled staging file into content-addressed blobs.

use std::fs::File;
use std::path::Path;

use fastcdc::v2020::StreamCDC;
use shared::RunnerError;

use super::chunk_io;
use super::manifest::{ChunkId, ChunkRef, Manifest};

/// FastCDC min/avg/max derived from the configured average (min = avg/4, max = avg*4).
fn cdc_sizes(avg: u32) -> Result<(usize, usize, usize), RunnerError> {
  let avg = usize::try_from(avg)
    .map_err(|e| RunnerError::Cache(format!("bad chunk avg: {e}")))?
    .max(4);
  Ok((avg / 4, avg, avg.saturating_mul(4)))
}

/// Chunk `staged` with FastCDC, write each unique chunk under `blobs_dir`, return the manifest.
/// Synchronous (blocking `StreamCDC`); call from `spawn_blocking`.
pub(crate) fn chunk_and_store(
  staged: &Path,
  blobs_dir: &Path,
  avg: u32,
) -> Result<Manifest, RunnerError> {
  let file = File::open(staged).map_err(RunnerError::Io)?;
  let (min, avg_size, max) = cdc_sizes(avg)?;
  let mut chunks = Vec::new();
  let mut total: u64 = 0;
  for result in StreamCDC::new(file, min, avg_size, max) {
    let chunk = result.map_err(|e| RunnerError::Cache(format!("fastcdc error: {e}")))?;
    let id = ChunkId(*blake3::hash(&chunk.data).as_bytes());
    let len = u32::try_from(chunk.data.len())
      .map_err(|e| RunnerError::Cache(format!("chunk too large: {e}")))?;
    let path = chunk_io::blob_path(blobs_dir, &id.to_hex());
    chunk_io::write_atomic_sync(&path, &chunk.data)?;
    total = total.saturating_add(u64::from(len));
    chunks.push(ChunkRef { id, len });
  }
  Ok(Manifest {
    chunks,
    total_size: total,
  })
}
