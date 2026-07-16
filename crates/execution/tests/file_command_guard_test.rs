//! FIX 8: the composite `$GITHUB_ENV` read-back must strip `NODE_OPTIONS`, like
//! the top-level file-command read-back. `strip_blocked_env` is the shared
//! guard both paths now use, so a composite `run:` step cannot set a node
//! preload for later composite node children. Real `$GITHUB_ENV` bodies.

use std::collections::HashMap;

use execution::execution::file_commands::{parse_env_file, strip_blocked_env};

#[test]
fn strip_blocked_env_drops_node_options_case_insensitively() {
  let mut env: HashMap<String, String> = [
    ("NODE_OPTIONS", "--require /tmp/evil.js"),
    ("node_options", "--require /tmp/evil2.js"),
    ("FOO", "bar"),
    ("PATH", "/usr/bin"),
  ]
  .into_iter()
  .map(|(k, v)| (k.to_owned(), v.to_owned()))
  .collect();

  strip_blocked_env(&mut env);

  assert!(
    !env.contains_key("NODE_OPTIONS"),
    "NODE_OPTIONS must be dropped"
  );
  assert!(
    !env.contains_key("node_options"),
    "NODE_OPTIONS must be dropped case-insensitively"
  );
  assert_eq!(
    env.get("FOO").map(String::as_str),
    Some("bar"),
    "unrelated vars must survive"
  );
  assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
}

#[test]
fn composite_env_readback_strips_node_options() {
  // Exercise the exact sequence composite `process_file_commands` now runs on a
  // real `$GITHUB_ENV` file body: parse, then strip the blocked key.
  let github_env = "NODE_OPTIONS=--require /tmp/evil.js\nGREETING=hi\n";
  let mut parsed = parse_env_file(github_env);
  strip_blocked_env(&mut parsed);

  assert!(
    !parsed.contains_key("NODE_OPTIONS"),
    "a composite run: step must not inject NODE_OPTIONS for later node children"
  );
  assert_eq!(parsed.get("GREETING").map(String::as_str), Some("hi"));
}
