//! Live log streaming via WebSocket — real-time log lines to GitHub Actions UI.
//!
//! Connects to the `FeedStreamUrl` WebSocket endpoint and sends log lines
//! as JSON `TimelineRecordFeedLinesWrapper` messages. Falls back silently
//! if connection fails (gzip blob upload is the durable path).

use std::collections::HashMap;
use std::time::Duration;

use futures_util::SinkExt;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const MIN_ATTEMPTS_BEFORE_THRESHOLD: u32 = 5;

/// Buffered log line with step context for WebSocket delivery.
#[derive(Debug)]
pub struct LiveLogLine {
  pub step_id: String,
  pub line: String,
}

/// JSON wrapper matching C# `TimelineRecordFeedLinesWrapper`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FeedLinesWrapper {
  step_id: String,
  lines: Vec<String>,
  start_line: u64,
}

/// Mutable state threaded through the flush loop.
struct SendState {
  line_counts: HashMap<String, u64>,
  total_attempts: u32,
  failed_attempts: u32,
  active: bool,
}

impl SendState {
  fn new() -> Self {
    Self {
      line_counts: HashMap::new(),
      total_attempts: 0,
      failed_attempts: 0,
      active: true,
    }
  }

  /// Record a failed send and disable if threshold exceeded.
  fn record_failure(&mut self) {
    self.failed_attempts += 1;
    if should_disable(self.total_attempts, self.failed_attempts) {
      tracing::warn!("disabling live log WebSocket after too many failures");
      self.active = false;
    }
  }
}

/// Actor that buffers log lines and flushes to WebSocket every 500ms.
pub struct LiveLogStreamer;

impl LiveLogStreamer {
  /// Connect to FeedStreamUrl and return (sender, join_handle).
  /// Returns None if connection fails — caller proceeds without live logs.
  pub async fn connect(
    feed_stream_url: &str,
    access_token: &str,
  ) -> Option<(mpsc::Sender<LiveLogLine>, tokio::task::JoinHandle<()>)> {
    let ws = open_websocket(feed_stream_url, access_token).await?;
    let (tx, rx) = mpsc::channel(4096);
    let handle = tokio::spawn(run_loop(ws, rx));
    Some((tx, handle))
  }
}

type WsStream =
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

enum WsConnectError {
  Timeout,
  Failed(tokio_tungstenite::tungstenite::Error),
}

/// Log a WebSocket connection error (kept separate to reduce complexity of callers).
fn log_ws_error(e: &WsConnectError) {
  match e {
    WsConnectError::Timeout => {
      tracing::warn!("live log WebSocket connection timed out, falling back");
    },
    WsConnectError::Failed(err) => {
      tracing::warn!(error = %err, "live log WebSocket connection failed, falling back");
    },
  }
}

/// Attempt to connect with a 30-second timeout, returning a typed error.
async fn try_connect(
  request: tokio_tungstenite::tungstenite::handshake::client::Request,
) -> Result<WsStream, WsConnectError> {
  let timed = tokio::time::timeout(
    Duration::from_secs(30),
    tokio_tungstenite::connect_async(request),
  )
  .await
  .map_err(|_elapsed| WsConnectError::Timeout)?;
  let (stream, _) = timed.map_err(WsConnectError::Failed)?;
  Ok(stream)
}

/// Build authenticated request and open WebSocket, returning the stream.
async fn open_websocket(feed_stream_url: &str, access_token: &str) -> Option<WsStream> {
  let mut request = feed_stream_url.into_client_request().ok()?;
  request.headers_mut().insert(
    "Authorization",
    format!("Bearer {access_token}").parse().ok()?,
  );
  match try_connect(request).await {
    Ok(stream) => {
      tracing::info!("live log WebSocket connected");
      Some(stream)
    },
    Err(e) => {
      log_ws_error(&e);
      None
    },
  }
}

/// Main event loop — buffer lines, flush to WebSocket every 500ms.
async fn run_loop<S>(mut ws: S, mut rx: mpsc::Receiver<LiveLogLine>)
where
  S: futures_util::Sink<Message> + Unpin,
  <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
  let mut buffer: Vec<LiveLogLine> = Vec::new();
  let mut state = SendState::new();
  let mut interval = tokio::time::interval(FLUSH_INTERVAL);
  interval.tick().await; // skip first immediate tick

  loop {
    tokio::select! {
      biased;
      msg = rx.recv() => {
        match msg {
          Some(line) => buffer.push(line),
          None => break, // channel closed
        }
      },
      _ = interval.tick(), if state.active => {
        flush(&mut ws, &mut buffer, &mut state).await;
      },
    }
  }

  // Final flush on shutdown
  if state.active {
    flush(&mut ws, &mut buffer, &mut state).await;
  }

  let _ = ws.close().await;
}

async fn flush<S>(ws: &mut S, buffer: &mut Vec<LiveLogLine>, state: &mut SendState)
where
  S: futures_util::Sink<Message> + Unpin,
  <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
  if buffer.is_empty() {
    return;
  }

  let grouped = drain_into_groups(buffer);
  for (step_id, lines) in grouped {
    send_step_lines(ws, state, step_id, lines).await;
    if !state.active {
      return;
    }
  }
}

/// Drain buffer and group lines by step_id.
fn drain_into_groups(buffer: &mut Vec<LiveLogLine>) -> HashMap<String, Vec<String>> {
  let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
  for line in buffer.drain(..) {
    grouped.entry(line.step_id).or_default().push(line.line);
  }
  grouped
}

/// Serialize and send one step's lines over WebSocket, updating state.
async fn send_step_lines<S>(ws: &mut S, state: &mut SendState, step_id: String, lines: Vec<String>)
where
  S: futures_util::Sink<Message> + Unpin,
  <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
  let start_line = state.line_counts.get(&step_id).copied().unwrap_or(1);
  let count = lines.len() as u64;
  let Some(json) = serialize_wrapper(&step_id, lines, start_line) else {
    return;
  };

  state.total_attempts += 1;
  if let Err(e) = ws.send(Message::Text(json.into())).await {
    tracing::warn!(
      error = %e,
      total = state.total_attempts,
      failed = state.failed_attempts + 1,
      "live log WebSocket send failed"
    );
    state.record_failure();
  } else {
    *state.line_counts.entry(step_id).or_insert(1) += count;
  }
}

/// Serialize lines into a JSON string, returning None on error.
fn serialize_wrapper(step_id: &str, lines: Vec<String>, start_line: u64) -> Option<String> {
  let wrapper = FeedLinesWrapper {
    step_id: step_id.to_owned(),
    lines,
    start_line,
  };
  match serde_json::to_string(&wrapper) {
    Ok(j) => Some(j),
    Err(e) => {
      tracing::warn!(error = %e, "live log serialize failed");
      None
    },
  }
}

fn should_disable(total: u32, failed: u32) -> bool {
  total >= MIN_ATTEMPTS_BEFORE_THRESHOLD && failed > total / 2
}
