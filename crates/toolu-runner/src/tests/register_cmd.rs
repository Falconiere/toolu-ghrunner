//! `register_config_path` (#208): where `register` puts a registration when
//! `--config` is absent — one dir per repository and per organization, so a
//! machine holds several of each and they never nest.

use std::path::Path;

use super::register_config_path;

const HOME: &str = "/home/ci/.toolu-runner";

#[test]
fn a_repository_url_gets_its_runners_dir() {
  for url in [
    "https://github.com/acme/web",
    "https://github.com/acme/web.git",
    "https://ghes.example.com/acme/web/",
  ] {
    assert_eq!(
      register_config_path(url, Path::new(HOME)).unwrap(),
      Path::new("/home/ci/.toolu-runner/runners/acme/web/config.toml"),
      "{url}"
    );
  }
}

#[test]
fn an_organization_url_gets_its_own_orgs_dir() {
  for (url, org) in [
    ("https://github.com/acme", "acme"),
    ("https://github.com/acme/", "acme"),
    ("https://ghes.example.com/beta", "beta"),
  ] {
    assert_eq!(
      register_config_path(url, Path::new(HOME)).unwrap(),
      Path::new(HOME).join("orgs").join(org).join("config.toml"),
      "{url}"
    );
  }
}

/// Two orgs and a repo of the same owner never share a dir.
#[test]
fn orgs_and_repos_of_one_owner_never_collide() {
  let home = Path::new(HOME);
  let paths = [
    register_config_path("https://github.com/acme", home).unwrap(),
    register_config_path("https://github.com/beta", home).unwrap(),
    register_config_path("https://github.com/acme/web", home).unwrap(),
    register_config_path("https://github.com/acme/api", home).unwrap(),
  ];
  for (i, a) in paths.iter().enumerate() {
    for b in paths.iter().skip(i + 1) {
      assert!(
        !a.starts_with(b.parent().unwrap()) && !b.starts_with(a.parent().unwrap()),
        "{a:?} vs {b:?}"
      );
    }
  }
}

#[test]
fn a_url_naming_nothing_is_refused() {
  let err = register_config_path("https://github.com/", Path::new(HOME))
    .unwrap_err()
    .to_string();
  assert!(err.contains("names no repository or organization"), "{err}");
}
