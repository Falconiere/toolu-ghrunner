//! Chunk ids, chunk refs, and the ranged-read manifest.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use shared::RunnerError;

/// A BLAKE3 digest identifying one content-addressed chunk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
  /// Lowercase 64-char hex rendering of the digest.
  pub fn to_hex(&self) -> String {
    blake3::Hash::from_bytes(self.0).to_hex().to_string()
  }

  /// Parse a 64-char lowercase hex digest.
  ///
  /// # Errors
  /// `RunnerError::Cache` when the string is not valid BLAKE3 hex.
  pub fn from_hex(hex: &str) -> Result<Self, RunnerError> {
    let hash =
      blake3::Hash::from_hex(hex).map_err(|e| RunnerError::Cache(format!("bad chunk id: {e}")))?;
    Ok(Self(*hash.as_bytes()))
  }
}

impl Serialize for ChunkId {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for ChunkId {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let hex = String::deserialize(deserializer)?;
    Self::from_hex(&hex).map_err(serde::de::Error::custom)
  }
}

/// One chunk reference: its id plus its byte length (reads are ranged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
  /// BLAKE3 id of the chunk's content.
  pub id: ChunkId,
  /// Length of the chunk in bytes.
  pub len: u32,
}

/// The ordered chunk list plus total assembled size of one blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
  /// Chunks in assembly order.
  pub chunks: Vec<ChunkRef>,
  /// Total size in bytes of the assembled blob.
  pub total_size: u64,
}

impl Manifest {
  /// Chunk index and intra-chunk offset holding byte `offset`, or `None` if out of range.
  ///
  /// Walks the chunk list accumulating a running end offset and returns on the
  /// first chunk that contains `offset` — no per-call allocation, and it stops
  /// as soon as the chunk is found rather than scanning the whole manifest.
  pub fn locate(&self, offset: u64) -> Option<(usize, u64)> {
    if offset >= self.total_size {
      return None;
    }
    let mut start = 0u64;
    for (idx, chunk) in self.chunks.iter().enumerate() {
      let end = start.saturating_add(u64::from(chunk.len));
      if offset < end {
        return Some((idx, offset - start));
      }
      start = end;
    }
    None
  }
}
