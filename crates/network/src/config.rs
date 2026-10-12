//! Environment reads for the `network` crate — the only `std::env` site here.

/// `HTTPS_PROXY` then `https_proxy` (reqwest's system-matcher order).
pub(crate) const HTTPS_PROXY: [&str; 2] = ["HTTPS_PROXY", "https_proxy"];
/// `HTTP_PROXY` then `http_proxy`.
pub(crate) const HTTP_PROXY: [&str; 2] = ["HTTP_PROXY", "http_proxy"];
/// `ALL_PROXY` then `all_proxy`.
pub(crate) const ALL_PROXY: [&str; 2] = ["ALL_PROXY", "all_proxy"];
/// `NO_PROXY` then `no_proxy`.
pub(crate) const NO_PROXY: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// The first of `names` that is set to a non-blank value, trimmed.
pub(crate) fn first_set(names: [&str; 2]) -> Option<String> {
  names.iter().find_map(|name| {
    std::env::var(name)
      .ok()
      .map(|value| value.trim().to_owned())
      .filter(|value| !value.is_empty())
  })
}
