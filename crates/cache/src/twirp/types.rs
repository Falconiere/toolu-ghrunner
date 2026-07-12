//! Snake_case JSON wire types for the three `CacheService` RPCs.
//!
//! Request fields are proto snake_case; unknown fields (notably `metadata`)
//! are ignored by serde's default. Response option fields are omitted when
//! absent, so a miss serializes to exactly `{"ok":false}` and a success omits
//! `message`. int64 fields cross the wire as decimal strings.

use serde::{Deserialize, Serialize};

/// `CreateCacheEntry` request: the opaque cache key and version.
#[derive(Debug, Deserialize)]
pub struct CreateRequest {
  /// Client-supplied, opaque cache key.
  pub key: String,
  /// Client-supplied, opaque cache version (never interpreted).
  pub version: String,
}

/// `CreateCacheEntry` response: a signed upload URL, or a refusal message.
#[derive(Debug, Serialize)]
pub struct CreateResponse {
  /// Whether the entry may be created.
  pub ok: bool,
  /// The blob URL to upload the archive to (present only when `ok`).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signed_upload_url: Option<String>,
  /// Why the create was refused (present only when not `ok`).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub message: Option<String>,
}

impl CreateResponse {
  /// A success carrying the signed upload URL.
  pub fn ok_upload(url: String) -> Self {
    Self {
      ok: true,
      signed_upload_url: Some(url),
      message: None,
    }
  }

  /// A refusal carrying an explanatory `message` (duplicate or write-denied).
  pub fn refused(message: String) -> Self {
    Self {
      ok: false,
      signed_upload_url: None,
      message: Some(message),
    }
  }
}

/// `FinalizeCacheEntryUpload` request: key, decimal-string size, and version.
#[derive(Debug, Deserialize)]
pub struct FinalizeRequest {
  /// Client-supplied, opaque cache key.
  pub key: String,
  /// Committed archive size in bytes, as a decimal string (int64 on the wire).
  pub size_bytes: String,
  /// Client-supplied, opaque cache version.
  pub version: String,
}

/// `FinalizeCacheEntryUpload` response: the entry id, or a bare failure.
#[derive(Debug, Serialize)]
pub struct FinalizeResponse {
  /// Whether the upload was ingested and indexed.
  pub ok: bool,
  /// The created entry id as a decimal string (present only when `ok`).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub entry_id: Option<String>,
}

impl FinalizeResponse {
  /// A success carrying the decimal-string entry id.
  pub fn ok(entry_id: String) -> Self {
    Self {
      ok: true,
      entry_id: Some(entry_id),
    }
  }

  /// A bare failure (`{"ok":false}`): size mismatch, unknown upload, or an
  /// ingest error. The job proceeds and rebuilds.
  pub fn failed() -> Self {
    Self {
      ok: false,
      entry_id: None,
    }
  }
}

/// `GetCacheEntryDownloadURL` request: key, restore-key prefixes, and version.
#[derive(Debug, Deserialize)]
pub struct DownloadRequest {
  /// The primary cache key (searched exactly first).
  pub key: String,
  /// Restore-key prefixes tried in order after the primary key misses.
  #[serde(default)]
  pub restore_keys: Vec<String>,
  /// Client-supplied, opaque cache version (matched exactly).
  pub version: String,
}

/// `GetCacheEntryDownloadURL` response: a signed URL + matched key, or a miss.
#[derive(Debug, Serialize)]
pub struct DownloadResponse {
  /// Whether an entry was found.
  pub ok: bool,
  /// The blob URL to download the archive from (present only on a hit).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signed_download_url: Option<String>,
  /// The key that actually matched (present only on a hit).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub matched_key: Option<String>,
}

impl DownloadResponse {
  /// A hit carrying the signed download URL and the matched key.
  pub fn hit(url: String, matched_key: String) -> Self {
    Self {
      ok: true,
      signed_download_url: Some(url),
      matched_key: Some(matched_key),
    }
  }

  /// A bare miss (`{"ok":false}`).
  pub fn miss() -> Self {
    Self {
      ok: false,
      signed_download_url: None,
      matched_key: None,
    }
  }
}
