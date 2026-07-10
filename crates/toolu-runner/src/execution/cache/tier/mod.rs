//! Optional cold-storage tiers behind the L1 CAS.

/// Optional S3 cold tier mirroring immutable chunks + manifests (never the index).
pub mod l2;

pub use l2::{BlobKind, L2Tier};
