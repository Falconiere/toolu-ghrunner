//! #94 AC-11: every production HTTP client starts from
//! `network::client_builder`, so `[network]` applies to all of them. Pins
//! that no production source outside `crates/network/src` constructs a
//! `reqwest` client directly (clippy's `disallowed-methods` would also hit the
//! ~150 test call sites, and `#[allow]` is banned).

use std::path::{Path, PathBuf};

/// Constructor spellings that bypass `network::client_builder`.
const CONSTRUCTORS: [&str; 3] = ["Client::builder(", "Client::new(", "ClientBuilder::new("];

/// Crates whose `src/` is not runner production code or is the builder itself.
const EXEMPT_CRATES: [&str; 2] = ["network", "daemon"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      // Test trees (`src/**/tests/`) may build plain clients.
      if path.file_name().is_some_and(|name| name == "tests") {
        continue;
      }
      rust_files(&path, out)?;
    } else if path.extension().is_some_and(|ext| ext == "rs") {
      out.push(path);
    }
  }
  Ok(())
}

/// Occurrences of a constructor not preceded by an identifier character
/// (`DockerClient::new(` is not a reqwest client).
fn raw_constructions(line: &str) -> bool {
  CONSTRUCTORS.iter().any(|needle| {
    line.match_indices(needle).any(|(at, _)| {
      line
        .get(..at)
        .and_then(|before| before.chars().next_back())
        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
  })
}

#[test]
fn no_production_source_builds_a_reqwest_client_directly() -> Result<(), Box<dyn std::error::Error>> {
  let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
  let mut offenders = Vec::new();
  let mut scanned = 0_usize;
  for entry in std::fs::read_dir(&crates_dir)? {
    let crate_dir = entry?.path();
    let name = crate_dir
      .file_name()
      .map(|n| n.to_string_lossy().into_owned())
      .unwrap_or_default();
    let src = crate_dir.join("src");
    if EXEMPT_CRATES.contains(&name.as_str()) || !src.is_dir() {
      continue;
    }
    let mut files = Vec::new();
    rust_files(&src, &mut files)?;
    for file in files {
      scanned += 1;
      let text = std::fs::read_to_string(&file)?;
      for (number, line) in text.lines().enumerate() {
        let code = line.trim_start();
        if !code.starts_with("//") && raw_constructions(code) {
          offenders.push(format!("{}:{}: {}", file.display(), number + 1, code));
        }
      }
    }
  }
  assert!(scanned > 100, "scanned only {scanned} files — wrong root?");
  assert!(
    offenders.is_empty(),
    "build HTTP clients with network::client_builder:\n{}",
    offenders.join("\n")
  );
  Ok(())
}

#[test]
fn the_scanner_recognises_what_it_bans() {
  assert!(raw_constructions("let c = reqwest::Client::builder()"));
  assert!(raw_constructions("client: reqwest::Client::new(),"));
  assert!(raw_constructions("Client::new()"));
  assert!(!raw_constructions("DockerClient::new(host)"));
  assert!(!raw_constructions("network::client_builder(&cfg)?"));
}
