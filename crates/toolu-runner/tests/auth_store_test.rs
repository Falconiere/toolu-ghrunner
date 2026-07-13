//! Unit tests for `config::auth_store`.
//!
//! Covers the pure precedence resolver (AC-4), the register-time TTY gate
//! (zero-arg-register AC-5), the per-host File backend with its `0600`
//! files and idempotent delete (AC-6), and env-sourced bearer resolution
//! (AC-3).
//!
//! No mocks: every test drives the real `AuthStore::File` backend against
//! a real `tempfile` directory. The File variant is constructed directly
//! so the suite never touches the OS keyring (hermetic on keyless CI).

use config::auth_store::{self, AuthStore, BearerDecision, StoredToken};
use tempfile::TempDir;

/// Build a `StoredToken` for `host` carrying `access`. `scope`/`issued_at`
/// are fixed literals — no field the tests assert on is left to chance.
fn token_for(host: &str, access: &str) -> StoredToken {
  StoredToken {
    access_token: access.to_owned(),
    scope: "repo,admin:org".to_owned(),
    host: host.to_owned(),
    issued_at: "2026-07-10T00:00:00+00:00".to_owned(),
  }
}

/// Fresh `File` store in a temp dir, pre-seeded with distinct tokens for
/// github.com and ghe.example.com. Uses `?` (not `expect`) so it stays
/// clippy-clean as a non-`#[test]` helper.
fn seeded_store() -> Result<(TempDir, AuthStore), Box<dyn std::error::Error>> {
  let dir = TempDir::new()?;
  let store = AuthStore::File(dir.path().to_path_buf());
  store.save(&token_for("github.com", "gho_dotcom"))?;
  store.save(&token_for("ghe.example.com", "gho_enterprise"))?;
  Ok((dir, store))
}

// ── AC-4: pick_bearer precedence (pure, no env, no I/O) ─────────────

#[test]
fn pick_bearer_prefers_flag_over_all() {
  let picked = auth_store::pick_bearer(
    Some("flag-token".to_owned()),
    Some("env-token".to_owned()),
    Some("stored-token".to_owned()),
  );
  assert_eq!(picked.as_deref(), Some("flag-token"));
}

#[test]
fn pick_bearer_prefers_env_over_stored() {
  let picked = auth_store::pick_bearer(
    None,
    Some("env-token".to_owned()),
    Some("stored-token".to_owned()),
  );
  assert_eq!(picked.as_deref(), Some("env-token"));
}

#[test]
fn pick_bearer_falls_back_to_stored() {
  let picked = auth_store::pick_bearer(None, None, Some("stored-token".to_owned()));
  assert_eq!(picked.as_deref(), Some("stored-token"));
}

#[test]
fn pick_bearer_none_when_all_absent() {
  assert_eq!(auth_store::pick_bearer(None, None, None), None);
}

// ── zero-arg-register AC-5: decide_bearer TTY gate (pure, no env, no I/O) ─

#[test]
fn decide_bearer_uses_resolved_token_on_tty() {
  assert_eq!(
    auth_store::decide_bearer(Some("resolved-token".to_owned()), true),
    BearerDecision::Use("resolved-token".to_owned()),
    "a resolved token must be used on a TTY"
  );
}

#[test]
fn decide_bearer_uses_resolved_token_without_tty() {
  assert_eq!(
    auth_store::decide_bearer(Some("resolved-token".to_owned()), false),
    BearerDecision::Use("resolved-token".to_owned()),
    "a resolved token must win regardless of TTY"
  );
}

#[test]
fn decide_bearer_starts_device_flow_when_none_on_tty() {
  assert_eq!(
    auth_store::decide_bearer(None, true),
    BearerDecision::StartDeviceFlow,
    "no token on a TTY must start the inline device flow"
  );
}

#[test]
fn decide_bearer_fail_message_names_all_manual_options() {
  let decision = auth_store::decide_bearer(None, false);
  assert!(
    matches!(decision, BearerDecision::Fail(_)),
    "no token without a TTY must be Fail; got {decision:?}"
  );
  let BearerDecision::Fail(msg) = decision else {
    return; // proven Fail by the assert above
  };
  for needle in ["--token", "TOOLU_RUNNER_TOKEN", "login"] {
    assert!(
      msg.contains(needle),
      "fail message must name {needle}; got: {msg}"
    );
  }
}

// ── AC-6: File backend — per-host, 0600, no clobber, idempotent delete ─

#[test]
fn file_backend_saves_distinct_files_per_host() {
  let (dir, store) = seeded_store().expect("seed store");

  // Two DISTINCT per-host files exist.
  let dotcom_file = dir.path().join("token-github.com.json");
  assert!(
    dotcom_file.exists(),
    "github.com token file missing at {}",
    dotcom_file.display()
  );
  assert!(
    dir.path().join("token-ghe.example.com.json").exists(),
    "ghe token file missing"
  );

  // Each load returns its own host's token — the second save did NOT
  // clobber the first.
  let dotcom = store
    .load("github.com")
    .expect("load github.com")
    .expect("github.com token present");
  assert_eq!(dotcom.access_token, "gho_dotcom");
  assert_eq!(dotcom.host, "github.com");
  assert_eq!(
    store
      .load("ghe.example.com")
      .expect("load ghe")
      .expect("ghe token present")
      .access_token,
    "gho_enterprise"
  );
}

#[test]
fn file_backend_load_unknown_host_is_none() {
  let (_dir, store) = seeded_store().expect("seed store");
  assert!(
    store
      .load("unknown.example.org")
      .expect("load unknown host")
      .is_none(),
    "unknown host must resolve to None, not an error"
  );
}

/// The per-host token file is `0600` (owner read/write only).
#[cfg(unix)]
#[test]
fn file_backend_token_file_is_0600() {
  use std::os::unix::fs::PermissionsExt;
  let (dir, _store) = seeded_store().expect("seed store");
  let mode = std::fs::metadata(dir.path().join("token-github.com.json"))
    .expect("token file metadata")
    .permissions()
    .mode();
  assert_eq!(
    mode & 0o777,
    0o600,
    "token file must be 0600; got {:o}",
    mode & 0o777
  );
}

#[test]
fn file_backend_delete_is_scoped_and_idempotent() {
  let (_dir, store) = seeded_store().expect("seed store");

  // delete → load is None.
  store.delete("github.com").expect("delete github.com");
  assert!(
    store
      .load("github.com")
      .expect("load after delete")
      .is_none(),
    "deleted host must resolve to None"
  );

  // A second delete is still Ok (idempotent).
  store
    .delete("github.com")
    .expect("second delete is a no-op");

  // Deleting github.com left the ghe token untouched.
  assert!(
    store
      .load("ghe.example.com")
      .expect("load ghe after github.com delete")
      .is_some(),
    "per-host delete must not remove other hosts"
  );
}

/// A host carrying path separators (`/` or `\`) is sanitized to a flat,
/// in-dir filename — it cannot spawn a subdirectory or climb out of the
/// data dir. Defensive against a hostile `--hostname` on any platform.
#[test]
fn file_backend_sanitizes_separators_in_host() {
  let dir = TempDir::new().expect("tempdir");
  let store = AuthStore::File(dir.path().to_path_buf());
  let host = "evil/../../etc\\passwd";

  store
    .save(&token_for(host, "gho_weird"))
    .expect("save separator host");

  // Round-trips through the same sanitized path.
  assert_eq!(
    store
      .load(host)
      .expect("load separator host")
      .expect("token present")
      .access_token,
    "gho_weird"
  );

  // Every entry directly under the data dir is a plain FILE — no separator
  // in the host created a nested directory or escaped upward.
  for entry in std::fs::read_dir(dir.path()).expect("read data dir") {
    let entry = entry.expect("dir entry");
    assert!(
      entry.file_type().expect("file type").is_file(),
      "unexpected non-file entry {:?} — host separators must not create dirs",
      entry.file_name()
    );
  }
}

/// A degenerate host whose sanitized form holds no alphanumeric (`..`, `::`)
/// still saves to a single flat file under the data dir and round-trips via
/// `load` — the fallback filename keeps it from becoming `token-.json` or a
/// dotted `token-...json` name.
#[test]
fn file_backend_handles_degenerate_host() {
  let dir = TempDir::new().expect("tempdir");
  let store = AuthStore::File(dir.path().to_path_buf());
  let host = "..";

  store
    .save(&token_for(host, "gho_degenerate"))
    .expect("save degenerate host");

  // Round-trips through the same fallback path.
  assert_eq!(
    store
      .load(host)
      .expect("load degenerate host")
      .expect("token present")
      .access_token,
    "gho_degenerate"
  );

  // Exactly one plain file exists directly under the data dir.
  let entries: Vec<_> = std::fs::read_dir(dir.path())
    .expect("read data dir")
    .map(|e| e.expect("dir entry"))
    .collect();
  assert_eq!(entries.len(), 1, "degenerate host must yield a single file");
  let entry = entries.first().expect("one entry");
  assert!(
    entry.file_type().expect("file type").is_file(),
    "degenerate host must not create a dir: {:?}",
    entry.file_name()
  );
}

// ── AC-3: resolve_bearer sources (flag > TOOLU_RUNNER_TOKEN env > store) ─
//
// This is the ONLY test that touches TOOLU_RUNNER_TOKEN. No other test in
// the suite reads it, so scoping it with `temp_env` (which serializes env
// access behind a global lock) cannot race any sibling test.

#[test]
fn resolve_bearer_prefers_flag_then_env_then_stored() {
  let dir = TempDir::new().expect("tempdir");
  let store = AuthStore::File(dir.path().to_path_buf());
  let host = "github.com";

  // Env set, store empty, no flag → the env value is used; a flag then
  // outranks that env value.
  temp_env::with_var("TOOLU_RUNNER_TOKEN", Some("env-token"), || {
    assert_eq!(
      auth_store::resolve_bearer(&store, host, None)
        .expect("resolve with env only")
        .as_deref(),
      Some("env-token"),
      "env token used when no flag and empty store"
    );
    assert_eq!(
      auth_store::resolve_bearer(&store, host, Some("flag-token".to_owned()))
        .expect("resolve with flag over env")
        .as_deref(),
      Some("flag-token"),
      "flag beats env"
    );
  });

  // Env unset + a stored token + no flag → the stored token is used.
  temp_env::with_var("TOOLU_RUNNER_TOKEN", None::<&str>, || {
    store
      .save(&token_for(host, "stored-token"))
      .expect("save stored token");
    assert_eq!(
      auth_store::resolve_bearer(&store, host, None)
        .expect("resolve from store")
        .as_deref(),
      Some("stored-token"),
      "stored token used when flag and env absent"
    );
  });
}
