//! `upgrade_url`: the `ws(s)://` → `http(s)://` rewrite of the feed URL.

use super::{WsConnectError, upgrade_url};

/// The rewritten URL as text, or the failure message.
fn rewritten(feed: &str) -> Result<String, String> {
  match upgrade_url(feed) {
    Ok(url) => Ok(url.to_string()),
    Err(WsConnectError::Failed(msg)) => Err(msg),
    Err(WsConnectError::Timeout) => Err("timeout".to_owned()),
  }
}

#[test]
fn websocket_schemes_become_their_http_counterparts() {
  for (feed, expected) in [
    ("ws://feed.test/a", "http://feed.test/a"),
    ("wss://feed.test/a", "https://feed.test/a"),
    (
      "wss://feed.test:8443/_apis/v1/feed/abc?token=1",
      "https://feed.test:8443/_apis/v1/feed/abc?token=1",
    ),
    ("http://feed.test/a", "http://feed.test/a"),
    ("https://feed.test/a", "https://feed.test/a"),
  ] {
    assert_eq!(rewritten(feed), Ok(expected.to_owned()), "{feed}");
  }
}

#[test]
fn unsupported_schemes_and_invalid_urls_are_errors() {
  assert_eq!(
    rewritten("ftp://feed.test/a"),
    Err("unsupported feed URL scheme ftp".to_owned())
  );
  let invalid = rewritten("not a url").err().unwrap_or_default();
  assert!(invalid.starts_with("invalid feed URL: "), "{invalid}");
  let empty = rewritten("").err().unwrap_or_default();
  assert!(empty.starts_with("invalid feed URL: "), "{empty}");
}
