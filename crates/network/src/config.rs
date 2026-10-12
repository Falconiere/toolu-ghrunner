//! Environment reads for the `network` crate — the only `std::env` site here.
//!
//! The proxy variables are read exactly as hyper-util's
//! `Matcher::from_system` reads them (every reqwest client in the runner uses
//! it in env mode): per pair the first *set* name wins, even when it is set
//! to an empty string, and `REQUEST_METHOD` disables all of them.

/// `HTTPS_PROXY` / `https_proxy`: the proxy for `https://` destinations.
pub(crate) fn https_proxy() -> Option<(&'static str, String)> {
  first_set("HTTPS_PROXY", "https_proxy")
}

/// `HTTP_PROXY` / `http_proxy`: the proxy for `http://` destinations.
pub(crate) fn http_proxy() -> Option<(&'static str, String)> {
  first_set("HTTP_PROXY", "http_proxy")
}

/// `ALL_PROXY` / `all_proxy`: the fallback proxy for both schemes.
pub(crate) fn all_proxy() -> Option<(&'static str, String)> {
  first_set("ALL_PROXY", "all_proxy")
}

/// `NO_PROXY` / `no_proxy`: the proxy bypass list.
pub(crate) fn no_proxy() -> Option<(&'static str, String)> {
  first_set("NO_PROXY", "no_proxy")
}

/// Whether `REQUEST_METHOD` is set: hyper-util then treats the process as a
/// CGI script and ignores every proxy variable (the httpoxy guard).
pub(crate) fn cgi_request() -> bool {
  std::env::var_os("REQUEST_METHOD").is_some()
}

/// The first *set* variable of a pair, with its name. A set-but-empty
/// upper-case name still wins (it disables the pair, as in hyper-util); a
/// value that is not valid Unicode counts as unset.
fn first_set(upper: &'static str, lower: &'static str) -> Option<(&'static str, String)> {
  read(upper)
    .map(|value| (upper, value))
    .or_else(|| read(lower).map(|value| (lower, value)))
}

fn read(name: &str) -> Option<String> {
  std::env::var(name).ok()
}
