//! Tests for `download`: tarball extraction (nodejs.org nesting, the
//! `actions/alpine_nodejs` `./` root, rejected escapes, permissions) and the
//! atomic staging promotion under real concurrency and over a stale dest.

use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::io::Write;
use std::sync::{Arc, Barrier};
use tar::{Builder, Header};

fn tmp_dest(label: &str) -> PathBuf {
  let mut p = env::temp_dir();
  p.push(format!(
    "toolu-node-runtime-test-{label}-{}",
    uuid::Uuid::new_v4()
  ));
  p
}

/// Build a tar.gz in memory containing the given entries (path, body,
/// unix mode). Mirrors `actions::downloader`'s test fixture builder.
fn build_tarball(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
  let mut raw = Vec::new();
  {
    let mut builder = Builder::new(&mut raw);
    for (path, body, mode) in entries {
      let mut header = Header::new_gnu();
      header.set_size(body.len() as u64);
      header.set_mode(*mode);
      header.set_cksum();
      builder
        .append_data(&mut header, path, *body)
        .expect("append entry");
    }
    builder.finish().expect("finish tar");
  }
  let mut gz = Vec::new();
  {
    let mut enc = GzEncoder::new(&mut gz, Compression::default());
    enc.write_all(&raw).expect("gzip write");
    enc.finish().expect("gzip finish");
  }
  gz
}

#[test]
fn extract_tarball_strips_prefix_and_preserves_permissions() {
  let dest = tmp_dest("extract");
  let tar = build_tarball(&[
    (
      "node-v20.20.2-linux-x64/bin/node",
      b"#!fake-node-binary",
      0o755,
    ),
    ("node-v20.20.2-linux-x64/README.md", b"node runtime", 0o644),
  ]);

  extract_tarball(&tar[..], &dest, 1).expect("tarball must extract");

  let node_bin = dest.join("bin").join("node");
  let content = std::fs::read(&node_bin).expect("binary present");
  assert_eq!(content, b"#!fake-node-binary");

  let readme = std::fs::read(dest.join("README.md")).expect("readme present");
  assert_eq!(readme, b"node runtime");

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&node_bin)
      .expect("metadata")
      .permissions()
      .mode();
    assert_eq!(
      mode & 0o777,
      0o755,
      "executable bit must survive extraction"
    );
  }
}

#[test]
fn extract_tarball_handles_the_alpine_dot_root_without_stripping() {
  let dest = tmp_dest("extract-alpine");
  // `actions/alpine_nodejs` tarballs list `./`, `./bin/`, `./bin/node`, `./LICENSE`.
  let tar = build_tarball(&[
    ("./bin/node", b"#!alpine-node", 0o755),
    ("./LICENSE", b"license", 0o644),
  ]);
  extract_tarball(&tar[..], &dest, 0).expect("alpine layout must extract");
  assert_eq!(
    std::fs::read(dest.join("bin/node")).expect("bin/node"),
    b"#!alpine-node"
  );
  assert_eq!(
    std::fs::read(dest.join("LICENSE")).expect("LICENSE"),
    b"license"
  );
}

#[test]
fn extract_tarball_rejects_an_entry_escaping_the_destination() {
  let dest = tmp_dest("extract-escape");
  let mut raw = Vec::new();
  {
    let mut builder = Builder::new(&mut raw);
    let mut header = Header::new_gnu();
    // `Header::set_path` refuses `..`, so write the raw name bytes.
    header.as_gnu_mut().expect("gnu header").name[..14].copy_from_slice(b"top/../../evil");
    header.set_size(4);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, &b"evil"[..]).expect("append");
    builder.finish().expect("finish");
  }
  let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
  gz.write_all(&raw).expect("gzip");
  let tar = gz.finish().expect("gzip finish");
  let error = extract_tarball(&tar[..], &dest, 1).expect_err("escape must be rejected");
  assert!(
    error.to_string().contains("escapes the runtime directory"),
    "{error}"
  );
  assert!(!dest.parent().expect("parent").join("evil").exists());
}

/// List any sibling staging dirs left behind for `dest` (i.e. anything
/// matching `<dest file name>.tmp-*` in `dest`'s parent directory). Mirrors
/// `actions::downloader`'s test helper of the same name.
fn leftover_staging_dirs(dest: &Path) -> Vec<PathBuf> {
  let Some(parent) = dest.parent() else {
    return Vec::new();
  };
  let Some(dest_name) = dest.file_name() else {
    return Vec::new();
  };
  let prefix = format!("{}.tmp-", dest_name.to_string_lossy());
  let Ok(entries) = std::fs::read_dir(parent) else {
    return Vec::new();
  };
  entries
    .filter_map(Result::ok)
    .map(|entry| entry.path())
    .filter(|path| {
      path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
    })
    .collect()
}

/// Two extractions racing `promote_staging` for the SAME `dest` (a
/// `std::sync::Barrier` forces genuine overlap on their blocking threads,
/// not a lucky interleave) must both return `Ok`, and `dest` must end up
/// holding exactly ONE complete extraction's content — never an
/// interleaving of both — with no `.tmp-` staging dirs left behind. This is
/// the core regression test for the atomic-staging fix applied to the node
/// runtime cache: two runner processes (different repos, always-online
/// loop) installing the same Node.js version share this cache dir.
#[tokio::test]
async fn concurrent_promotions_to_the_same_dest_yield_one_complete_tree() {
  let dest = tmp_dest("node-concurrent-dest");
  let staging_a = staging_dir_for(&dest);
  let staging_b = staging_dir_for(&dest);
  let tar_a = build_tarball(&[("node-v20.20.2-linux-x64/bin/node", b"#!fake-node-a", 0o755)]);
  let tar_b = build_tarball(&[("node-v20.20.2-linux-x64/bin/node", b"#!fake-node-b", 0o755)]);
  extract_tarball(&tar_a[..], &staging_a, 1).expect("extract into staging a");
  extract_tarball(&tar_b[..], &staging_b, 1).expect("extract into staging b");

  let gate = Arc::new(Barrier::new(2));

  let gate_a = Arc::clone(&gate);
  let dest_a = dest.clone();
  let handle_a = tokio::task::spawn_blocking(move || {
    gate_a.wait();
    promote_staging(&staging_a, &dest_a)
  });

  let gate_b = Arc::clone(&gate);
  let dest_b = dest.clone();
  let handle_b = tokio::task::spawn_blocking(move || {
    gate_b.wait();
    promote_staging(&staging_b, &dest_b)
  });

  let (result_a, result_b) = tokio::join!(handle_a, handle_b);
  result_a
    .expect("promote task a must join")
    .expect("promote a must succeed");
  result_b
    .expect("promote task b must join")
    .expect("promote b must succeed");

  let content =
    std::fs::read(node_binary_path(&dest)).expect("binary present exactly once at dest");
  assert!(
    content == b"#!fake-node-a" || content == b"#!fake-node-b",
    "dest must hold exactly one complete extraction's content, got: {content:?}"
  );
  assert_eq!(
    leftover_staging_dirs(&dest),
    Vec::<PathBuf>::new(),
    "no staging dirs must remain after both promotions settle"
  );
}

/// A stale `dest` left by a pre-staging-fix (or interrupted) extraction —
/// present on disk but missing the binary — must be replaced rather than
/// causing `promote_staging` to error out.
#[test]
fn promote_staging_replaces_a_stale_dest_missing_the_binary() {
  let dest = tmp_dest("node-stale-dest");
  std::fs::create_dir_all(&dest).expect("mkdir stale dest");
  std::fs::write(dest.join("leftover.txt"), b"partial").expect("write stale leftover file");
  assert!(!node_binary_path(&dest).exists(), "precondition: no binary");

  let staging = staging_dir_for(&dest);
  let tar = build_tarball(&[("node-v20.20.2-linux-x64/bin/node", b"#!fresh-node", 0o755)]);
  extract_tarball(&tar[..], &staging, 1).expect("extract into staging");

  promote_staging(&staging, &dest).expect("promote over a stale dest must succeed");

  let content = std::fs::read(node_binary_path(&dest)).expect("fresh binary present");
  assert_eq!(content, b"#!fresh-node");
  assert!(
    !dest.join("leftover.txt").exists(),
    "the stale leftover file must not survive the replace"
  );
  assert_eq!(
    leftover_staging_dirs(&dest),
    Vec::<PathBuf>::new(),
    "no staging dir must remain after a successful promote"
  );
}
