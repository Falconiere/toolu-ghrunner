//! Optional S3 cold tier: mirrors immutable CAS chunks + manifests, never the index.

use opendal::{Operator, services};
use shared::{L2Config, RunnerError};

/// Which content-addressed namespace an S3 object lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
  /// A FastCDC content chunk, keyed under `chunks/<hex>`.
  Chunk,
  /// A serialized manifest, keyed under `manifests/<hex>`.
  Manifest,
}

impl BlobKind {
  /// S3 key prefix for this namespace.
  fn prefix(self) -> &'static str {
    match self {
      Self::Chunk => "chunks/",
      Self::Manifest => "manifests/",
    }
  }
}

/// S3-backed cold tier mirroring immutable chunks + manifests (content-addressed,
/// so a re-put of the same id is idempotent). Cloning yields a second handle to
/// the same bucket — the underlying `Operator` is cheap to clone.
#[derive(Clone)]
pub struct L2Tier {
  op: Operator,
}

impl L2Tier {
  /// Build an S3-backed tier from `cfg`. Credentials come from the standard AWS
  /// environment (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`), which opendal
  /// loads; bucket / endpoint / region come from `cfg`.
  ///
  /// # Errors
  /// `RunnerError::Cache` if the opendal S3 operator cannot be constructed.
  pub fn from_config(cfg: &L2Config) -> Result<Self, RunnerError> {
    let builder = services::S3::default()
      .bucket(&cfg.bucket)
      .endpoint(&cfg.endpoint)
      .region(&cfg.region);
    let op = Operator::new(builder)
      .map_err(|e| RunnerError::Cache(format!("failed to build S3 operator: {e}")))?
      .finish();
    Ok(Self { op })
  }

  /// Content-addressed S3 key for `hex_id` in the `kind` namespace.
  fn key(kind: BlobKind, hex_id: &str) -> String {
    format!("{}{hex_id}", kind.prefix())
  }

  /// Mirror `bytes` to `<kind>/<hex_id>` in the bucket (idempotent overwrite).
  ///
  /// # Errors
  /// `RunnerError::Cache` if the S3 write fails.
  pub async fn put_blob(
    &self,
    kind: BlobKind,
    hex_id: &str,
    bytes: &[u8],
  ) -> Result<(), RunnerError> {
    let key = Self::key(kind, hex_id);
    self
      .op
      .write(&key, bytes.to_vec())
      .await
      .map_err(|e| RunnerError::Cache(format!("L2 put {key} failed: {e}")))
  }

  /// Fetch `<kind>/<hex_id>` from the bucket; `Ok(None)` when the object is absent.
  ///
  /// # Errors
  /// `RunnerError::Cache` on any S3 error other than not-found.
  pub async fn get_blob(
    &self,
    kind: BlobKind,
    hex_id: &str,
  ) -> Result<Option<Vec<u8>>, RunnerError> {
    let key = Self::key(kind, hex_id);
    match self.op.read(&key).await {
      Ok(buf) => Ok(Some(buf.to_vec())),
      Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(RunnerError::Cache(format!("L2 get {key} failed: {e}"))),
    }
  }

  /// True if `<kind>/<hex_id>` exists in the bucket.
  ///
  /// # Errors
  /// `RunnerError::Cache` on any S3 error.
  pub async fn has_blob(&self, kind: BlobKind, hex_id: &str) -> Result<bool, RunnerError> {
    let key = Self::key(kind, hex_id);
    self
      .op
      .exists(&key)
      .await
      .map_err(|e| RunnerError::Cache(format!("L2 stat {key} failed: {e}")))
  }
}
