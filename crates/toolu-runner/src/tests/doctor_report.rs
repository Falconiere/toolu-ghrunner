//! Tests for the `doctor` report model: row layout, fix lines, masking, the
//! summary line and exit-code aggregation (verbatim `check` lines included).

use shared::SecretMasker;

use super::{EXIT_FAILED, Level, Row, Section, exit_code, render};

fn sections() -> Vec<Section> {
  vec![
    Section::new(
      "install",
      vec![Row::new(Level::Ok, "version", "toolu-runner 0.11.1")],
    ),
    Section::new(
      "registrations",
      vec![
        Row::new(Level::Fail, "run", "not running")
          .with_fix("toolu-runner install-service --config /h/config.toml"),
        Row::new(Level::Warn, "creds", "mode 0644 (group/other can read)")
          .with_fix("chmod 600 /h/credentials.json"),
        Row::new(Level::Info, "last job", "no jobs yet"),
      ],
    ),
  ]
}

#[test]
fn rows_render_with_padded_tags_names_and_fix_lines() {
  let text = render("box-1", &sections(), &SecretMasker::new());
  let expected = "\
toolu-runner doctor — box-1
install:
  OK    version    toolu-runner 0.11.1
registrations:
  FAIL  run        not running
        fix: toolu-runner install-service --config /h/config.toml
  WARN  creds      mode 0644 (group/other can read)
        fix: chmod 600 /h/credentials.json
  INFO  last job   no jobs yet
result: 1 failure, 1 warning (exit 1)
";
  assert_eq!(text, expected);
}

#[test]
fn a_failure_exits_1_and_warnings_alone_exit_0() {
  assert_eq!(exit_code(&sections()), EXIT_FAILED);
  let warn_only = vec![Section::new(
    "host",
    vec![
      Row::new(Level::Warn, "user", "running as root (uid 0)"),
      Row::new(Level::Skip, "update", "offline"),
    ],
  )];
  assert_eq!(exit_code(&warn_only), 0);
  let text = render("h", &warn_only, &SecretMasker::new());
  assert!(
    text.ends_with("result: 0 failures, 1 warning (exit 0)\n"),
    "{text}"
  );
}

#[test]
fn verbatim_lines_carry_their_own_failure_and_warning_counts() {
  let network = Section::verbatim(
    "network (github.com)",
    vec![
      "  network (required):".to_owned(),
      "    FAIL  api  …".to_owned(),
    ],
    2,
    3,
  );
  assert_eq!(exit_code(std::slice::from_ref(&network)), EXIT_FAILED);
  let text = render("h", &[network], &SecretMasker::new());
  assert!(
    text.contains("network (github.com):\n  network (required):\n    FAIL  api  …\n"),
    "{text}"
  );
  assert!(
    text.ends_with("result: 2 failures, 3 warnings (exit 1)\n"),
    "{text}"
  );
}

#[test]
fn every_line_passes_through_the_masker() {
  let mut masker = SecretMasker::new();
  masker.add_secret("s3cr3t-bearer-value");
  let leaky = vec![Section::new(
    "github auth (github.com)",
    vec![
      Row::new(Level::Warn, "token", "echoed s3cr3t-bearer-value")
        .with_fix("unset s3cr3t-bearer-value"),
    ],
  )];
  let text = render("h", &leaky, &masker);
  assert!(!text.contains("s3cr3t-bearer-value"), "{text}");
  assert!(
    text.contains("  WARN  token      echoed ***\n        fix: unset ***\n"),
    "{text}"
  );
}
