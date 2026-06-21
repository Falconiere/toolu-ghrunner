//! Live console feed (in-progress log) tests for `reporting::live_log`.
//!
//! GitHub renders a step's logs WHILE it runs from the live console feed:
//! `TimelineRecordFeedLinesWrapper` frames streamed over the `FeedStreamUrl`
//! WebSocket (and the V2 Results-Service equivalent), batched on a line-count
//! OR time threshold. The durable gzip step-log blob is still committed once
//! at step end — it is NOT the live-render path (it is a single-shot BlockBlob
//! PUT, which Azure cannot render incrementally).
//!
//! These tests drive the REAL flush loop (`live_log::run_loop`) against an
//! in-process `Sink<Message>` collector — a real sink, not a mock of the
//! flush logic. They assert:
//!   1. the wire frame matches the C# `TimelineRecordFeedLinesWrapper` shape
//!      (`value` for the lines, `count`, `stepId`, `startLine`), and
//!   2. a burst larger than the count threshold produces MULTIPLE frames
//!      BEFORE the line channel closes (incremental flush by count), and the
//!      concatenation of every frame's `value` array equals the full ordered
//!      input — no dropped, duplicated, or reordered lines.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::Sink;
use serde_json::Value;
use tokio::sync::mpsc;

use toolu_runner::reporting::live_log::{FLUSH_LINE_THRESHOLD, LiveLogLine, Message, run_loop};

/// In-process `Sink<Message>` that records every text frame the streamer
/// sends. A real sink — the actual `run_loop`/flush/serialize code path runs
/// against it; nothing about the flush logic is mocked.
#[derive(Clone, Default)]
struct CollectingSink {
  frames: Arc<Mutex<Vec<String>>>,
}

impl CollectingSink {
  fn snapshot(&self) -> Vec<String> {
    match self.frames.lock() {
      Ok(g) => g.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  fn push_frame(&self, text: String) {
    match self.frames.lock() {
      Ok(mut g) => g.push(text),
      Err(poisoned) => poisoned.into_inner().push(text),
    }
  }
}

impl Sink<Message> for CollectingSink {
  type Error = Infallible;

  fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
    Poll::Ready(Ok(()))
  }

  fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Infallible> {
    if let Message::Text(text) = item {
      self.push_frame(text.to_string());
    }
    Ok(())
  }

  fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
    Poll::Ready(Ok(()))
  }

  fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
    Poll::Ready(Ok(()))
  }
}

/// A parsed live-console frame in C# `TimelineRecordFeedLinesWrapper` shape.
struct Frame {
  count: u64,
  start_line: u64,
  step_id: String,
  values: Vec<String>,
  has_lines_key: bool,
}

/// Parse one collected JSON frame, returning `Err(reason)` if any C# wire
/// field is missing or mistyped. The caller surfaces the error so the lint
/// gate's no-panic rule holds for these shared helpers.
fn parse_frame(frame: &str) -> Result<Frame, String> {
  let v: Value = serde_json::from_str(frame).map_err(|e| format!("invalid JSON: {e}"))?;
  let count = v
    .get("count")
    .and_then(Value::as_u64)
    .ok_or_else(|| format!("missing u64 `count`: {frame}"))?;
  let start_line = v
    .get("startLine")
    .and_then(Value::as_u64)
    .ok_or_else(|| format!("missing u64 `startLine`: {frame}"))?;
  let step_id = v
    .get("stepId")
    .and_then(Value::as_str)
    .ok_or_else(|| format!("missing `stepId`: {frame}"))?
    .to_owned();
  let arr = v
    .get("value")
    .and_then(Value::as_array)
    .ok_or_else(|| format!("missing `value` array: {frame}"))?;
  let mut values = Vec::with_capacity(arr.len());
  for item in arr {
    let line = item
      .as_str()
      .ok_or_else(|| format!("line is not a string: {frame}"))?;
    values.push(line.to_owned());
  }
  Ok(Frame {
    count,
    start_line,
    step_id,
    values,
    has_lines_key: v.get("lines").is_some(),
  })
}

/// Feed `total` ordered lines for one step through `run_loop`, returning the
/// collected frames and the frame count observed at channel-close. All lines
/// are pre-queued and drained via the `biased` select in microseconds, well
/// before the 500ms timer's first real tick, so every pre-close frame is a
/// count-threshold flush. (The final-reconstruction assertion holds even if a
/// stray timer tick interleaves, since an empty-buffer flush is a no-op.)
///
/// Returns `Err(reason)` on a channel/join failure so the caller surfaces it.
async fn run_burst(step_id: &str, total: usize) -> Result<(Vec<String>, usize), String> {
  let sink = CollectingSink::default();
  let (tx, rx) = mpsc::channel::<LiveLogLine>(8192);

  for i in 0..total {
    tx.send(LiveLogLine {
      step_id: step_id.to_owned(),
      line: format!("line-{i}"),
    })
    .await
    .map_err(|e| format!("send line {i}: {e}"))?;
  }

  let collector = sink.clone();
  let handle = tokio::spawn(run_loop(sink, rx));

  // Let the actor drain the queued lines (biased select drains rx first).
  for _ in 0..=total {
    tokio::task::yield_now().await;
  }
  let frames_before_close = collector.snapshot().len();

  // Close the channel; the shutdown path flushes the remainder.
  drop(tx);
  handle.await.map_err(|e| format!("run_loop join: {e}"))?;

  Ok((collector.snapshot(), frames_before_close))
}

#[tokio::test]
async fn frame_uses_csharp_wire_field_names() {
  // One full threshold batch produces at least one frame; pin its shape.
  let (frames, _) = run_burst("step-1", FLUSH_LINE_THRESHOLD)
    .await
    .expect("burst runs");
  let first = frames.first().expect("at least one frame");
  let parsed = parse_frame(first).expect("frame parses with C# field names");

  // GitHub's console reads `value` (the lines) and `count`. An array under
  // any other key (e.g. the old broken `lines`) is silently dropped and
  // nothing renders live.
  assert_eq!(parsed.step_id, "step-1");
  assert!(
    !parsed.has_lines_key,
    "stale broken `lines` key present: {first}"
  );
  assert_eq!(
    parsed.count,
    parsed.values.len() as u64,
    "count must equal value length"
  );
}

#[tokio::test]
async fn count_threshold_flushes_incrementally_before_close() {
  // 2.5x the threshold = >= 2 full count-threshold batches before close,
  // plus a remainder flushed on shutdown.
  let total = FLUSH_LINE_THRESHOLD * 2 + FLUSH_LINE_THRESHOLD / 2;
  let (frames, frames_before_close) = run_burst("step-1", total).await.expect("burst runs");

  assert!(
    frames_before_close >= 2,
    "expected >= 2 count-threshold flushes BEFORE close, got {frames_before_close}"
  );
  assert!(
    frames.len() > frames_before_close,
    "expected a final remainder flush on close: before={frames_before_close} total={}",
    frames.len()
  );

  // Concatenation of every frame's ordered values == full ordered input,
  // and `startLine` advances so the UI appends rather than overwrites.
  let expected: Vec<String> = (0..total).map(|i| format!("line-{i}")).collect();
  let mut reconstructed: Vec<String> = Vec::new();
  let mut next_start = 1u64;
  for frame in &frames {
    let parsed = parse_frame(frame).expect("frame parses");
    assert_eq!(
      parsed.start_line, next_start,
      "startLine must continue from prior frame"
    );
    next_start += parsed.values.len() as u64;
    reconstructed.extend(parsed.values);
  }
  assert_eq!(
    reconstructed, expected,
    "flushed chunks must reconstruct the full ordered line set"
  );
  assert_eq!(next_start, total as u64 + 1, "all lines accounted for");
}
