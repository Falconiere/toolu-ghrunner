//! In-memory blob token registry: opaque 256-bit nonces mapping to an upload
//! staging target or a download manifest, each with a TTL.
//!
//! A token is a random 256-bit value (never persisted), strictly stronger than
//! a disk-readable HMAC key under the same-uid threat model. Expired or absent
//! tokens resolve to `None`, which every handler renders as `403` — the
//! recoverable "re-request the URL" signal BuildKit expects.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as TOKEN;
use rand::RngCore;

use crate::execution::cache::cas::Manifest;

/// Fallback TTL when the requested one overflows `Instant` arithmetic: far
/// enough out that no real client notices, finite so the entry still ages out.
const OVERFLOW_TTL_CAP: Duration = Duration::from_secs(24 * 60 * 60);

/// What a minted blob token authorizes.
#[derive(Debug, Clone)]
pub enum BlobTarget {
  /// Write path: raw bytes stage to `staging`; Twirp Finalize later ingests them.
  Upload {
    /// Path the assembled object is written to.
    staging: PathBuf,
    /// Cache scope (branch ref) the eventual entry belongs to.
    scope: String,
    /// Client-supplied, opaque cache key.
    key: String,
    /// Client-supplied, opaque cache version.
    version: String,
  },
  /// Read path: HEAD/GET stream from this manifest's chunks.
  Download {
    /// Manifest describing the chunks to serve.
    manifest: Manifest,
  },
}

/// One live token: its target plus the instant it stops resolving.
struct Entry {
  target: BlobTarget,
  expiry: Instant,
}

/// Thread-safe registry of live blob tokens with a per-entry TTL.
///
/// Cloning shares the same backing map (Arc), so the Twirp layer that mints
/// tokens and the blob router that resolves them see one registry.
#[derive(Clone, Default)]
pub struct BlobRegistry {
  entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl BlobRegistry {
  /// Create an empty registry.
  pub fn new() -> Self {
    Self::default()
  }

  /// Mint an upload token whose object stages to `staging`; returns the token.
  pub fn mint_upload(
    &self,
    staging: PathBuf,
    scope: String,
    key: String,
    version: String,
    ttl: Duration,
  ) -> String {
    let target = BlobTarget::Upload {
      staging,
      scope,
      key,
      version,
    };
    self.insert(target, ttl)
  }

  /// Mint a download token serving `manifest`; returns the token.
  pub fn mint_download(&self, manifest: Manifest, ttl: Duration) -> String {
    self.insert(BlobTarget::Download { manifest }, ttl)
  }

  /// Resolve a token to its target, or `None` if it is missing or expired.
  pub fn get(&self, token: &str) -> Option<BlobTarget> {
    let map = self.entries.lock().ok()?;
    let entry = map.get(token)?;
    if entry.expiry <= Instant::now() {
      return None;
    }
    Some(entry.target.clone())
  }

  /// Consume an upload token, returning `(staging, scope, key, version)`.
  ///
  /// Removes the entry regardless of kind; a download token yields `None`.
  /// Twirp Finalize calls this once, then ingests the staged object.
  pub fn take_upload(&self, token: &str) -> Option<(PathBuf, String, String, String)> {
    let mut map = self.entries.lock().ok()?;
    let entry = map.remove(token)?;
    if entry.expiry <= Instant::now() {
      return None;
    }
    match entry.target {
      BlobTarget::Upload {
        staging,
        scope,
        key,
        version,
      } => Some((staging, scope, key, version)),
      BlobTarget::Download { .. } => None,
    }
  }

  /// Consume the pending upload for `(scope, key, version)`, returning its
  /// token and staging path.
  ///
  /// Twirp `FinalizeCacheEntryUpload` resolves an upload by its cache
  /// coordinates — the client never echoes the blob token back — so this
  /// searches the live uploads rather than taking a token. Expired entries and
  /// download tokens are ignored; the matched entry is removed.
  pub fn take_pending_upload(
    &self,
    scope: &str,
    key: &str,
    version: &str,
  ) -> Option<(String, PathBuf)> {
    let mut map = self.entries.lock().ok()?;
    let now = Instant::now();
    let mut found: Option<String> = None;
    for (tok, entry) in map.iter() {
      if entry.expiry <= now {
        continue;
      }
      if let BlobTarget::Upload {
        scope: s,
        key: k,
        version: v,
        ..
      } = &entry.target
        && s == scope
        && k == key
        && v == version
      {
        found = Some(tok.clone());
        break;
      }
    }
    let token = found?;
    let entry = map.remove(&token)?;
    match entry.target {
      BlobTarget::Upload { staging, .. } => Some((token, staging)),
      BlobTarget::Download { .. } => None,
    }
  }

  /// Store `target` under a fresh nonce with the given TTL; returns the nonce.
  fn insert(&self, target: BlobTarget, ttl: Duration) -> String {
    let token = mint_token();
    if let Ok(mut map) = self.entries.lock() {
      // A TTL too large for `Instant` arithmetic caps at `OVERFLOW_TTL_CAP`
      // instead of silently minting an already-expired token.
      let now = Instant::now();
      let expiry = now
        .checked_add(ttl)
        .or_else(|| now.checked_add(OVERFLOW_TTL_CAP))
        .unwrap_or(now);
      map.insert(token.clone(), Entry { target, expiry });
    }
    token
  }
}

/// Generate an opaque token from 32 CSPRNG bytes, base64url-encoded.
///
/// The token is a bearer capability, so it must come from a CSPRNG —
/// `rand::thread_rng` is ChaCha reseeded from OS entropy, unlike the
/// predictable `fastrand` used for non-secret poll jitter.
fn mint_token() -> String {
  let mut bytes = [0u8; 32];
  rand::thread_rng().fill_bytes(&mut bytes);
  TOKEN.encode(bytes)
}
