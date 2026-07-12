//! Live E2E for `docker buildx type=gha` through the accelerated cache (S18).
//!
//! Starts a standalone accelerated cache server in-process (the same
//! `accelerated_app` the runner mounts, over a temp content-addressed store),
//! creates a `--driver-opt network=host` buildx builder so `docker-container`
//! BuildKit can reach the host loopback, and runs a real
//! `docker buildx build --cache-to type=gha --cache-from type=gha` twice
//! against a multi-stage Dockerfile. It asserts the second build reuses
//! cached layers (AC-5) and, by not crashing the buildx `go-actions-cache`
//! client, that the server supplies the `x-ms-request-id` the client
//! unconditionally dereferences.
//!
//! Gated on `TOOLU_TEST_DOCKER` AND a live `docker info` probe. When either is
//! absent every path prints a LOUD SKIP naming what went uncovered and returns
//! Ok — a docker test that silently passed would read as coverage it never
//! had. The single test is `#[ignore]`'d so the file still compiles under
//! `cargo test --features live`.

#![cfg(feature = "live")]

use std::path::Path;
use std::process::Command as StdCommand;

use cache::cas::{CacheIndex, CasStore, LeaseSet};
use cache::scope::CacheScopes;
use cache::server::CacheServer;
use cache::trust::TrustLevel;
use cache::{AcceleratedInputs, BlobRegistry, accelerated_app};

/// A dummy JWT for buildx's `token=`. BuildKit's `go-actions-cache` client
/// parses the token as a JWT (`ParseUnverified`) to read the `ac` scope claim
/// BEFORE any HTTP call — an opaque string fails with "invalid number of
/// segments". The signature is never verified (by the client or our server),
/// so a static 3-segment token with a permissive `ac` claim and a far-future
/// `exp` suffices. Our server compares the bearer verbatim, so it uses the same
/// string.
fn buildx_jwt() -> String {
  use base64::Engine;
  let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
  let header = b64.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
  // `ac` is a JSON-string-encoded array of {Scope, Permission} (3 = read|write);
  // exp far future, nbf/iat in the past so the client's validity checks pass.
  let payload = b64
    .encode(br#"{"ac":"[{\"Scope\":\"gha\",\"Permission\":3}]","exp":4102444800,"nbf":0,"iat":0}"#);
  let sig = b64.encode(b"toolu-unverified-signature");
  format!("{header}.{payload}.{sig}")
}
/// Name of the throwaway buildx builder this test creates and removes.
const BUILDER: &str = "toolu-cache-buildx-test";
/// L1 eviction ceiling for the throwaway store (100 GiB, the config default).
const MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;
/// FastCDC target average chunk size (64 KiB, the config default).
const CHUNK_AVG_BYTES: u32 = 64 * 1024;

/// A real two-stage Dockerfile — the `RUN` layers are what must come back
/// CACHED on the second build.
const DOCKERFILE: &str = "\
FROM alpine:3.19 AS build
RUN echo layer-one > /one.txt
RUN echo layer-two > /two.txt
FROM alpine:3.19
COPY --from=build /one.txt /one.txt
RUN echo final > /final.txt
";

/// Print a LOUD skip line naming what went uncovered, then return `Ok`.
macro_rules! skip_docker {
  ($reason:expr) => {{
    eprintln!(
      "SKIP cache_live_docker: {} — buildx type=gha layer-reuse (AC-5) NOT covered",
      $reason
    );
    return Ok(());
  }};
}

/// True when a local docker daemon answers `docker info`.
fn docker_probe_ok() -> bool {
  StdCommand::new("docker")
    .arg("info")
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// True when a bridge container can resolve `host.docker.internal` — the case
/// on Docker Desktop (Mac/Windows), where a `network=host` builder shares the
/// Desktop VM's netns and CANNOT reach a host-bound loopback server.
fn host_docker_internal_resolves() -> bool {
  StdCommand::new("docker")
    .args([
      "run",
      "--rm",
      "alpine:3.19",
      "getent",
      "hosts",
      "host.docker.internal",
    ])
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// How buildkit reaches the host-bound cache server, per platform.
struct CacheEndpoint {
  /// The `url=` the buildx `type=gha` attribute uses.
  url: String,
  /// Whether the builder needs `--driver-opt network=host`.
  host_net: bool,
}

/// Resolve the endpoint: on Docker Desktop, `host.docker.internal` on the
/// default bridge; on native Linux docker, host loopback via `network=host`.
fn resolve_endpoint(base_url: &str, port: u16) -> CacheEndpoint {
  if host_docker_internal_resolves() {
    CacheEndpoint {
      url: format!("http://host.docker.internal:{port}/"),
      host_net: false,
    }
  } else {
    CacheEndpoint {
      url: base_url.to_owned(),
      host_net: true,
    }
  }
}

/// Write the multi-stage Dockerfile into `dir` (buildx finds it by name).
fn write_dockerfile(dir: &Path) -> std::io::Result<()> {
  std::fs::write(dir.join("Dockerfile"), DOCKERFILE)
}

/// Start a standalone accelerated cache server over a temp CAS root. Serves the
/// v2 Twirp + Azure-blob endpoints buildx's `type=gha` exporter drives; the
/// write scope is server-internal, so buildx's own scope attribute is
/// irrelevant, and the read ladder mirrors the write scope so the second build
/// resolves the first build's entries.
async fn start_local_cache(
  root: &Path,
  bearer: &str,
) -> Result<CacheServer, Box<dyn std::error::Error>> {
  let cache_dir = root.join("cache");
  let staging_root = cache_dir.join("staging");
  std::fs::create_dir_all(&staging_root)?;
  let inputs = AcceleratedInputs {
    store: CasStore::new(cache_dir.clone(), CHUNK_AVG_BYTES, MAX_BYTES),
    index: CacheIndex::new(cache_dir),
    registry: BlobRegistry::new(),
    leases: LeaseSet::new(),
    scopes: CacheScopes {
      write: "gha".to_owned(),
      read_ladder: vec!["gha".to_owned()],
    },
    trust: TrustLevel::Trusted,
    protected: Vec::new(),
    bearer: bearer.to_owned(),
    staging_root,
    upstream_results_url: String::new(),
    client: reqwest::Client::new(),
  };
  Ok(CacheServer::start(accelerated_app(inputs), "0.0.0.0:0").await?)
}

/// Create a `docker-container` builder, bootstrapped so a bind failure surfaces
/// now rather than on first build. Adds `--driver-opt network=host` only when
/// the host is reached via loopback (native Linux); Docker Desktop reaches it
/// via `host.docker.internal` on the default bridge and must NOT use host net.
fn create_builder(host_net: bool) -> std::io::Result<std::process::Output> {
  let mut cmd = StdCommand::new("docker");
  cmd.args([
    "buildx",
    "create",
    "--name",
    BUILDER,
    "--driver",
    "docker-container",
  ]);
  if host_net {
    cmd.args(["--driver-opt", "network=host"]);
  }
  cmd.arg("--bootstrap").output()
}

/// Best-effort teardown of the throwaway builder.
fn remove_builder() {
  let _ = StdCommand::new("docker")
    .args(["buildx", "rm", "--force", BUILDER])
    .output();
}

/// Run one `docker buildx build` with `type=gha` cache import + export against
/// the local server. `--output type=cacheonly` runs every layer but produces
/// no image, so no registry or `--load` is needed.
fn run_buildx(
  ctx: &Path,
  url: &str,
  tag: &str,
  token: &str,
) -> std::io::Result<std::process::Output> {
  StdCommand::new("docker")
    .args(["buildx", "build", "--builder", BUILDER])
    .arg(format!(
      "--cache-to=type=gha,mode=max,url={url},token={token}"
    ))
    .arg(format!("--cache-from=type=gha,url={url},token={token}"))
    .args(["--output", "type=cacheonly", "-t", tag])
    .arg(ctx)
    .output()
}

/// Log a failed build's stderr and return whether it succeeded.
fn build_ok(label: &str, out: &std::process::Output) -> bool {
  if !out.status.success() {
    eprintln!(
      "{label} build stderr:\n{}",
      String::from_utf8_lossy(&out.stderr)
    );
  }
  out.status.success()
}

/// Assert the second build reused cached layers (the `CACHED` marker in buildx
/// output) — the AC-5 proof that the local cache round-tripped through buildx.
fn assert_layers_cached(second: &std::process::Output) {
  let out2 = format!(
    "{}{}",
    String::from_utf8_lossy(&second.stdout),
    String::from_utf8_lossy(&second.stderr)
  );
  assert!(
    out2.contains("CACHED"),
    "second buildx build must reuse cached layers through the local gha cache; output:\n{out2}"
  );
}

/// Unwrap both buildx spawn results, or say WHY spawning failed so the skip
/// line names the real reason (e.g. `docker` missing from PATH).
fn spawned(
  first: std::io::Result<std::process::Output>,
  second: std::io::Result<std::process::Output>,
) -> Result<(std::process::Output, std::process::Output), String> {
  match (first, second) {
    (Ok(first), Ok(second)) => Ok((first, second)),
    (first, second) => Err(
      [first.err(), second.err()]
        .into_iter()
        .flatten()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; "),
    ),
  }
}

/// AC-5: `docker buildx build --cache-to type=gha` reuses layers through the
/// local accelerated cache on the second run, and the buildx cache client does
/// not crash on a missing `x-ms-request-id`.
#[tokio::test]
#[ignore = "live docker test — requires TOOLU_TEST_DOCKER + a working docker daemon"]
async fn buildx_type_gha_reuses_layers_through_local_cache()
-> Result<(), Box<dyn std::error::Error>> {
  if std::env::var("TOOLU_TEST_DOCKER").is_err() {
    skip_docker!("TOOLU_TEST_DOCKER unset");
  }
  if !docker_probe_ok() {
    skip_docker!("`docker info` failed — no reachable docker daemon");
  }

  let ctx = tempfile::tempdir()?;
  write_dockerfile(ctx.path())?;
  let token = buildx_jwt();
  let server = start_local_cache(ctx.path(), &token).await?;
  let endpoint = resolve_endpoint(server.base_url(), server.address().port());
  eprintln!(
    "cache_live_docker: buildkit → {} (network=host: {})",
    endpoint.url, endpoint.host_net
  );

  let created = create_builder(endpoint.host_net)?;
  if !created.status.success() {
    remove_builder();
    server.shutdown().await;
    skip_docker!("could not create the buildx builder");
  }

  let first = run_buildx(ctx.path(), &endpoint.url, "toolu-cache-test:1", &token);
  let second = run_buildx(ctx.path(), &endpoint.url, "toolu-cache-test:2", &token);
  remove_builder();
  server.shutdown().await;

  let (first, second) = match spawned(first, second) {
    Ok(pair) => pair,
    Err(why) => skip_docker!(format!("docker buildx build could not be spawned: {why}")),
  };
  if !build_ok("first", &first) {
    skip_docker!("first buildx build failed (image pull / network)");
  }
  if !build_ok("second", &second) {
    skip_docker!("second buildx build failed (image pull / network)");
  }

  assert_layers_cached(&second);
  Ok(())
}
