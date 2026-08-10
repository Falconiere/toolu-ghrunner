//! Tests for `docker::client` endpoint resolution.
//!
//! Real data: the endpoint strings real Docker-compatible daemons publish —
//! Docker Desktop's default socket, a Colima socket under the user's home,
//! `OrbStack`'s, and a TCP daemon. No mocks; `resolve_docker_host` is pure, so
//! the values below are the whole input space that matters.

use shared::RunnerError;

// The production constant itself, not a copy of its value: a local literal
// would keep asserting the old default after a change to `client.rs` and pass.
use super::{DEFAULT_DOCKER_ENDPOINT, DockerClient, resolve_docker_host};

/// Unset / blank must land on the Docker CLI's own default rather than an
/// empty endpoint.
#[test]
fn absent_or_blank_docker_host_falls_back_to_the_default_socket() {
  assert_eq!(resolve_docker_host(None), DEFAULT_DOCKER_ENDPOINT);
  assert_eq!(resolve_docker_host(Some("")), DEFAULT_DOCKER_ENDPOINT);
  assert_eq!(resolve_docker_host(Some("   ")), DEFAULT_DOCKER_ENDPOINT);
}

/// The endpoints Colima, `OrbStack`, Podman and a remote TCP daemon actually
/// export pass through untouched — this is the regression that made the runner
/// blind to every non-Docker-Desktop setup on macOS.
#[test]
fn a_real_docker_host_is_passed_through_verbatim() {
  for endpoint in [
    "unix:///Users/dev/.colima/default/docker.sock",
    "unix:///Users/dev/.orbstack/run/docker.sock",
    "unix:///run/user/1000/podman/podman.sock",
    "tcp://127.0.0.1:2375",
    "http://192.168.64.2:2375",
  ] {
    assert_eq!(resolve_docker_host(Some(endpoint)), endpoint);
  }
}

/// Surrounding whitespace (a trailing newline from a shell export) must not
/// become part of the endpoint.
#[test]
fn surrounding_whitespace_is_trimmed() {
  assert_eq!(
    resolve_docker_host(Some("  tcp://127.0.0.1:2375\n")),
    "tcp://127.0.0.1:2375"
  );
}

/// `resolve_docker_host` is a projection, not a parser: feeding it its own
/// output must not change the endpoint.
#[test]
fn resolution_is_idempotent() {
  let once = resolve_docker_host(Some("unix:///Users/dev/.colima/default/docker.sock"));
  assert_eq!(resolve_docker_host(Some(&once)), once);
}

/// A scheme bollard was not built with must fail loudly, naming the endpoint —
/// silently falling back to the default socket would run the job against a
/// daemon the operator did not choose.
///
/// `temp_env` scopes the mutation. The write is process-global, but a test
/// binary is its own process and nothing else linked into THIS one reads
/// `DOCKER_HOST` — another crate's tests run in a separate process and cannot
/// observe it.
#[test]
fn an_unsupported_scheme_errors_naming_the_endpoint() {
  temp_env::with_var("DOCKER_HOST", Some("ssh://builder@10.0.0.5"), || {
    // One match, no dead arm: only `Err(RunnerError::Docker(_))` yields a bare
    // message, so every other outcome is carried into the assertion as its own
    // `Debug` rendering — which starts with `Ok(`/`Err(` and can never satisfy
    // the prefix below. That pins the variant and the text in one assertion
    // instead of a `matches!` guard plus an unreachable fallback.
    let message = match DockerClient::new() {
      Err(RunnerError::Docker(message)) => message,
      other => format!("{other:?}"),
    };
    // Anchored at the front, not a bare `contains`: the endpoint has to be the
    // one the message is ABOUT, not a substring that happens to appear inside
    // bollard's own trailing text.
    assert!(
      message.starts_with("connect docker daemon at ssh://builder@10.0.0.5:"),
      "ssh:// is outside bollard's default features — construction must fail \
       with RunnerError::Docker naming the endpoint; got: {message:?}"
    );
  });
}
