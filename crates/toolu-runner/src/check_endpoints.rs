//! `check` endpoint selection: which GitHub service URLs to probe for a
//! target, mirroring upstream `ActionsCheck.cs` (actions/runner `cab9d1c`)
//! with the registration's own JIT endpoints taking precedence.
//!
//! Pure — no I/O. The default github.com health paths were confirmed live
//! (each answers `200`).

use url::{Host, Url};

/// What `check` does for one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Probe {
  /// GET this URL.
  Get(String),
  /// Not applicable for this target, with the reason printed.
  Skip(&'static str),
}

/// One GitHub service endpoint in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
  /// Stable short name printed in the report (`api`, `broker`, …).
  pub(crate) name: &'static str,
  /// A failure makes the whole check fail (exit 1) when `true`.
  pub(crate) required: bool,
  /// The probe to run.
  pub(crate) probe: Probe,
}

/// The service URLs a registration's JIT config names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JitEndpoints {
  /// `.runner` `ServerUrl` — the pipelines service.
  pub(crate) pipelines: String,
  /// `.runner` `ServerUrlV2` — the broker (V2) service.
  pub(crate) broker: String,
  /// `.credentials` `AuthorizationUrl` — the OAuth token service.
  pub(crate) token: String,
}

/// Where Node runtimes are downloaded from (`execution::node::runtime`).
const NODE_DIST: &str = "https://nodejs.org/dist/";

/// Parse a `--url` / `runner_url` value: `http` or `https` with a host.
///
/// # Errors
///
/// A message naming the problem when the value does not parse, uses another
/// scheme, or has no host.
pub(crate) fn parse_target_url(raw: &str) -> Result<Url, String> {
  let url = Url::parse(raw).map_err(|e| format!("invalid GitHub URL {raw:?}: {e}"))?;
  if !matches!(url.scheme(), "http" | "https") {
    return Err(format!(
      "invalid GitHub URL {raw:?}: scheme must be http or https, got {}",
      url.scheme()
    ));
  }
  if url.host_str().is_none_or(str::is_empty) {
    return Err(format!("invalid GitHub URL {raw:?}: no host"));
  }
  Ok(url)
}

/// Read the pipelines / broker / token URLs out of an encoded JIT config.
///
/// # Errors
///
/// The parse failure, without the value (it holds credentials).
pub(crate) fn jit_endpoints(encoded: &str) -> Result<JitEndpoints, String> {
  // The parse error is dropped on purpose: serde's messages can quote field
  // values from the decoded credential blobs.
  let jit = protocol::JitConfig::parse(encoded)
    .ok()
    .ok_or_else(|| "the registration's jit_config is not a valid JIT config envelope".to_owned())?;
  Ok(JitEndpoints {
    pipelines: jit.runner_settings.server_url,
    broker: jit.runner_settings.server_url_v2,
    token: jit.credentials.data.authorization_url,
  })
}

/// Upstream `UrlUtil.IsHostedServer`: github.com and GHE.com tenants.
fn is_hosted(host: &str) -> bool {
  let host = host.to_ascii_lowercase();
  host == "github.com" || host == "www.github.com" || host.ends_with(".ghe.com")
}

/// The target host, classified the way upstream derives service URLs.
struct Site<'a> {
  /// The `--url` / `runner_url` value.
  base: &'a Url,
  /// The host as it appears in a URL (IPv6 bracketed).
  host: String,
  /// The host without a leading `www.` (hosted only).
  apex: String,
  /// github.com or a GHE.com tenant.
  hosted: bool,
  /// An IPv4/IPv6 literal (no subdomains possible).
  ip_literal: bool,
}

impl<'a> Site<'a> {
  fn new(base: &'a Url) -> Self {
    let raw = base.host_str().unwrap_or_default();
    let host = match base.host() {
      Some(Host::Ipv6(addr)) => format!("[{addr}]"),
      _ => raw.to_owned(),
    };
    Self {
      base,
      apex: raw.strip_prefix("www.").unwrap_or(raw).to_owned(),
      hosted: is_hosted(raw),
      ip_literal: matches!(base.host(), Some(Host::Ipv4(_) | Host::Ipv6(_))),
      host,
    }
  }

  /// `scheme://<host>[:port]<path>` on the target's scheme and port.
  fn on(&self, host: &str, path: &str) -> String {
    let port = self
      .base
      .port()
      .map(|port| format!(":{port}"))
      .unwrap_or_default();
    format!("{}://{host}{port}{path}", self.base.scheme())
  }

  /// A hosted static URL, or `<target>/<ghes_path>` on GHES.
  fn service(&self, hosted_url: &str, ghes_path: &str) -> String {
    if self.hosted {
      hosted_url.to_owned()
    } else {
      self.on(&self.host, ghes_path)
    }
  }

  fn api(&self) -> String {
    self.service(&format!("https://api.{}/", self.apex), "/api/v3")
  }

  fn codeload(&self) -> Probe {
    if self.hosted {
      Probe::Get(format!("https://codeload.{}/_ping", self.apex))
    } else if self.ip_literal {
      Probe::Skip("IP-literal host: no codeload subdomain exists (archives come from the host)")
    } else {
      Probe::Get(self.on(&format!("codeload.{}", self.host), "/_ping"))
    }
  }

  /// The registration's broker URL; on GHES a V1 registration repeats the
  /// pipelines URL there, which means "no broker".
  fn broker(&self, jit: Option<&JitEndpoints>) -> Probe {
    match jit.filter(|j| !j.broker.is_empty()) {
      Some(j) if self.hosted || j.broker != j.pipelines => Probe::Get(j.broker.clone()),
      _ if self.hosted => {
        Probe::Get("https://broker.actions.githubusercontent.com/health".to_owned())
      },
      _ => Probe::Skip("GHES uses the V1 protocol: no broker service"),
    }
  }

  fn results(&self) -> Probe {
    if self.hosted {
      Probe::Get("https://results-receiver.actions.githubusercontent.com/health".to_owned())
    } else {
      Probe::Skip("not applicable on GHES (no Results service)")
    }
  }
}

/// A non-empty JIT-config URL, if the registration supplied one.
fn configured(jit: Option<&JitEndpoints>, pick: fn(&JitEndpoints) -> &String) -> Option<String> {
  jit.map(pick).filter(|url| !url.is_empty()).cloned()
}

/// A required endpoint.
fn required(name: &'static str, probe: Probe) -> Endpoint {
  Endpoint {
    name,
    required: true,
    probe,
  }
}

/// Every endpoint `check` reports for `base`, in report order.
pub(crate) fn endpoints_for(base: &Url, jit: Option<&JitEndpoints>) -> Vec<Endpoint> {
  let site = Site::new(base);
  let pipelines = configured(jit, |j| &j.pipelines).unwrap_or_else(|| {
    site.service(
      "https://pipelines.actions.githubusercontent.com/_apis/health",
      "/_services/pipelines/_apis/health",
    )
  });
  let token = configured(jit, |j| &j.token).unwrap_or_else(|| {
    site.service(
      "https://vstoken.actions.githubusercontent.com/_apis/health",
      "/_services/vstoken/_apis/health",
    )
  });
  vec![
    required("api", Probe::Get(site.api())),
    required("codeload", site.codeload()),
    required("pipelines", Probe::Get(pipelines)),
    required("broker", site.broker(jit)),
    required("token", Probe::Get(token)),
    required("results", site.results()),
    Endpoint {
      name: "node-dist",
      required: false,
      probe: Probe::Get(NODE_DIST.to_owned()),
    },
  ]
}

#[cfg(test)]
#[path = "tests/check_endpoints.rs"]
mod tests;
