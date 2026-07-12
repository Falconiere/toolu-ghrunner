//! Real-data unit tests for the CAS `Manifest`: `locate()` and JSON round-trip.

use cache::cas::manifest::{ChunkId, ChunkRef, Manifest};

/// A deterministic 32-byte id whose first byte marks which chunk it is.
fn id(tag: u8) -> ChunkId {
  let mut bytes = [0u8; 32];
  bytes[0] = tag;
  ChunkId(bytes)
}

/// Manifest of chunks 10, 20, 30, 40 bytes long (total 100).
fn sample() -> Manifest {
  Manifest {
    chunks: vec![
      ChunkRef { id: id(1), len: 10 },
      ChunkRef { id: id(2), len: 20 },
      ChunkRef { id: id(3), len: 30 },
      ChunkRef { id: id(4), len: 40 },
    ],
    total_size: 100,
  }
}

#[test]
fn locate_at_chunk_boundaries() {
  let m = sample();
  // First byte of each chunk: cumulative starts 0, 10, 30, 60.
  assert_eq!(m.locate(0), Some((0, 0)));
  assert_eq!(m.locate(10), Some((1, 0)));
  assert_eq!(m.locate(30), Some((2, 0)));
  assert_eq!(m.locate(60), Some((3, 0)));
}

#[test]
fn locate_mid_chunk() {
  let m = sample();
  assert_eq!(m.locate(5), Some((0, 5)));
  assert_eq!(m.locate(15), Some((1, 5)));
  assert_eq!(m.locate(45), Some((2, 15)));
  assert_eq!(m.locate(99), Some((3, 39)));
}

#[test]
fn locate_out_of_range_is_none() {
  let m = sample();
  assert_eq!(m.locate(100), None);
  assert_eq!(m.locate(1000), None);
}

#[test]
fn locate_empty_manifest_is_none() {
  let m = Manifest {
    chunks: vec![],
    total_size: 0,
  };
  assert_eq!(m.locate(0), None);
}

#[test]
fn json_round_trips() -> Result<(), Box<dyn std::error::Error>> {
  let m = sample();
  let json = serde_json::to_string(&m)?;
  let back: Manifest = serde_json::from_str(&json)?;
  assert_eq!(m, back);
  Ok(())
}

#[test]
fn chunk_id_serializes_as_lowercase_hex() -> Result<(), Box<dyn std::error::Error>> {
  let m = Manifest {
    chunks: vec![ChunkRef {
      id: id(0xab),
      len: 4,
    }],
    total_size: 4,
  };
  let json = serde_json::to_string(&m)?;
  // 0xab followed by 31 zero bytes → "ab" + 62 zeros, all lowercase.
  assert!(json.contains("\"ab00000000000000000000000000000000000000000000000000000000000000\""));
  assert_eq!(json, json.to_lowercase());
  Ok(())
}
