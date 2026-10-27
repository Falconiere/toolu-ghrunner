//! setup's `/dev/tty` reader thread, and lending its input to an async line
//! reader (#205): while `create-app` waits inside setup for a pasted code,
//! the thread forwards the bytes there instead of to the prompts, so two
//! readers never race for one terminal.

use std::io::Read as _;
use std::pin::Pin;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader, ReadBuf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::create_app_cmd::TermLines;

/// Where the reader thread sends bytes while a reader is lent.
pub(crate) type Redirect = Arc<Mutex<Option<UnboundedSender<u8>>>>;

/// Start the detached `/dev/tty` reader thread: each byte goes to the lent
/// reader while one is alive, else to the prompts (`tx`). It cannot be
/// joined without closing the tty under a live prompt; it exits on EOF, a
/// read error, or when the prompts are gone, and lives at most for this
/// process.
pub(crate) fn spawn_reader(mut reader: std::fs::File, tx: Sender<u8>, redirect: Redirect) {
  std::thread::spawn(move || {
    let mut buf = [0_u8; 64];
    loop {
      let Ok(n) = reader.read(&mut buf) else {
        return;
      };
      if n == 0 {
        return;
      }
      for byte in buf.iter().take(n) {
        let lent = redirect
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .as_ref()
          .is_some_and(|lend| lend.send(*byte).is_ok());
        if !lent && tx.send(*byte).is_err() {
          return;
        }
      }
    }
  });
}

/// Lend the terminal's input as line input. Dropping the lines hands it back
/// to the prompts (the thread's next send fails over to them).
pub(crate) fn lend(redirect: &Redirect) -> TermLines {
  let (tx, rx) = unbounded_channel();
  *redirect.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
  let reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send> =
    Box::new(BufReader::new(Lent { rx }));
  reader.lines()
}

/// The lent bytes as an async byte stream.
struct Lent {
  rx: UnboundedReceiver<u8>,
}

impl AsyncRead for Lent {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<std::io::Result<()>> {
    if buf.remaining() == 0 {
      return Poll::Ready(Ok(()));
    }
    match self.rx.poll_recv(cx) {
      // Closed: EOF.
      Poll::Ready(None) => Poll::Ready(Ok(())),
      Poll::Ready(Some(byte)) => {
        buf.put_slice(&[byte]);
        while buf.remaining() > 0 {
          let Ok(next) = self.rx.try_recv() else {
            break;
          };
          buf.put_slice(&[next]);
        }
        Poll::Ready(Ok(()))
      },
      Poll::Pending => Poll::Pending,
    }
  }
}

#[cfg(test)]
#[path = "tests/tty_lines.rs"]
mod tests;
