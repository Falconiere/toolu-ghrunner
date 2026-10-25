//! Plan grammar: real flag sets in, resolved plan or a flag-naming
//! exit-2 error out.

use super::*;
use crate::setup::args::SetupArgs;

fn parsed(extra: &[&str]) -> SetupArgs {
  use clap::Parser as _;
  let mut argv = vec!["toolu-runner", "setup"];
  argv.extend(extra.iter().copied());
  let cli = crate::cli::Cli::try_parse_from(argv).expect("setup flags parse");
  let crate::cli::Cli {
    command: crate::cli::Command::Setup(parsed_args),
  } = cli
  else {
    return parsed(&[]);
  };
  parsed_args
}

#[test]
fn github_url_defaults_and_validates() {
  assert_eq!(validate_github_url(None).unwrap(), "https://github.com");
  assert_eq!(
    validate_github_url(Some("http://127.0.0.1:8080/")).unwrap(),
    "http://127.0.0.1:8080"
  );
  assert_eq!(
    validate_github_url(Some("https://ghes.example.com")).unwrap(),
    "https://ghes.example.com"
  );
  for bad in [
    "http://ghes.example.com",
    "https://github.com/acme",
    "https://user@github.com",
    "not a url",
  ] {
    let err = validate_github_url(Some(bad)).unwrap_err();
    assert_eq!(err.code, 2, "{bad}");
    assert!(
      err.message.contains("--github-url"),
      "{bad}: {}",
      err.message
    );
  }
}

#[test]
fn flag_target_rejects_several_owners() {
  let a = parsed(&["--repo", "acme/web", "--repo", "other/api"]);
  let err = flag_target(&a).unwrap_err();
  assert_eq!(err.code, 2);
  assert!(err.message.contains("--repo"));
}

#[test]
fn flag_target_org_and_repos() {
  let a = parsed(&["--org", "acme"]);
  assert_eq!(
    flag_target(&a).unwrap(),
    Some(Target::Org {
      org: "acme".to_owned()
    })
  );
  let a = parsed(&["--repo", "acme/web"]);
  assert_eq!(
    flag_target(&a).unwrap(),
    Some(Target::Repos {
      repos: vec!["acme/web".to_owned()]
    })
  );
  let a = parsed(&[]);
  assert_eq!(flag_target(&a).unwrap(), None);
}

#[test]
fn auth_grammar_names_missing_flags() {
  let a = parsed(&["--auth", "import"]);
  let err = auth_plan(&a, false).unwrap_err();
  assert!(err.message.contains("--import"), "{}", err.message);
  let a = parsed(&["--auth", "token", "--import", "x.json"]);
  let err = auth_plan(&a, false).unwrap_err();
  assert_eq!(err.code, 2);
  let a = parsed(&[]);
  assert_eq!(auth_plan(&a, true).unwrap(), AuthPlan::SavedApp);
  assert_eq!(auth_plan(&a, false).unwrap(), AuthPlan::CreateApp);
}

#[test]
fn github_url_never_echoes_credentials() {
  let err = validate_github_url(Some("http://user:s3cr3t-pw@ghes.example.com")).unwrap_err();
  assert!(
    !err.message.contains("s3cr3t-pw"),
    "password echoed: {}",
    err.message
  );
  assert!(
    !err.message.contains("user:"),
    "userinfo echoed: {}",
    err.message
  );
}
