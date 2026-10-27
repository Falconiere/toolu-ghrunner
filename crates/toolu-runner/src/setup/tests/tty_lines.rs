//! The lent reader gets the bytes while it lives; the prompts get them back
//! once it is dropped (#205).

use std::sync::Arc;
use std::sync::mpsc::channel;

use super::{Redirect, lend};

/// Forward `bytes` as the reader thread does: the lent reader first, else
/// the prompts' channel.
fn forward(redirect: &Redirect, prompts: &std::sync::mpsc::Sender<u8>, bytes: &[u8]) {
  for byte in bytes {
    let lent = redirect
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .as_ref()
      .is_some_and(|lend| lend.send(*byte).is_ok());
    if !lent {
      assert!(prompts.send(*byte).is_ok(), "prompts alive");
    }
  }
}

#[tokio::test]
async fn a_lent_reader_takes_the_lines_and_hands_back_on_drop() {
  let redirect = Redirect::default();
  let (tx, rx) = channel::<u8>();
  let mut lines = lend(&redirect);
  forward(&redirect, &tx, b"p\nc0de\n");
  assert_eq!(lines.next_line().await.expect("read").as_deref(), Some("p"));
  assert_eq!(
    lines.next_line().await.expect("read").as_deref(),
    Some("c0de")
  );
  assert!(rx.try_recv().is_err(), "the prompts saw a lent byte");
  drop(lines);
  forward(&redirect, &tx, b"\r");
  assert_eq!(rx.try_recv().ok(), Some(b'\r'));
  assert!(Arc::strong_count(&redirect) == 1);
}
