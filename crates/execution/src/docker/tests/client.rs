//! Tests for `docker::client` endpoint resolution.
//!
//! Real data: the endpoint strings real Docker-compatible daemons publish —
//! Docker Desktop's default socket, a Colima socket under the user's home,
//! `OrbStack`'s, and a TCP daemon. No mocks; `resolve_docker_host` is pure, so
//! the values below are the whole input space that matters.

use shared::RunnerError;

use super::{DockerClient, resolve_docker_host};

const DEFAULT_SOCKET: &str = "unix:///var/run/docker.sock";

/// Unset / blank must land on the Docker CLI's own default rather than an
/// empty endpoint.
#[test]
fn absent_or_blank_docker_host_falls_back_to_the_default_socket() {
  assert_eq!(resolve_docker_host(None), DEFAULT_SOCKET);
  assert_eq!(resolve_docker_host(Some("")), DEFAULT_SOCKET);
  assert_eq!(resolve_docker_host(Some("   ")), DEFAULT_SOCKET);
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
/// `temp_env` scopes the mutation; nothing else in this crate reads
/// `DOCKER_HOST`, so the global write cannot race another test.
#[test]
fn an_unsupported_scheme_errors_naming_the_endpoint() {
  temp_env::with_var("DOCKER_HOST", Some("ssh://builder@10.0.0.5"), || {
    // Match the variant, not just the text: a different `RunnerError` carrying
    // the endpoint by coincidence would otherwise satisfy this.
    let result = DockerClient::new();
    assert!(
      matches!(result, Err(RunnerError::Docker(_))),
      "ssh:// is outside bollard's default features — construction must fail \
       with RunnerError::Docker; got: {result:?}"
    );
    // The assertion above already fixed the variant; the fallback arm is dead.
    let message = match result {
      Err(RunnerError::Docker(message)) => message,
      _ => String::new(),
    };
    assert!(
      message.contains("ssh://builder@10.0.0.5"),
      "the error must name the endpoint; got: {message:?}"
    );
  });
}
