//! Tests for `prefetch`: `ActionFetcher::ensure`'s single-flight dedupe and
//! evict-on-failure semantics, `prefetch_refs`'s dedupe-via-`resolve_action_refs`
//! wiring and WARN-and-swallow failure handling, and `spawn_prefetch`'s
//! background-task plumbing — all against a real local HTTP server (AC-10),
//! mirroring `downloader.rs`'s real-server test pattern.

use super::*;
use crate::execution::actions::downloader::is_action_cached;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tar::{Builder, Header};

fn tmp_dest(label: &str) -> PathBuf {
  let mut p = env::temp_dir();
  p.push(format!(
    "toolu-prefetch-test-{label}-{}",
    uuid::Uuid::new_v4()
  ));
  p
}

/// Build a tar.gz in memory containing a single entry with the given
/// tar-path (mirrors `downloader.rs`'s test helper).
fn build_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
  let mut raw = Vec::new();
  {
    let mut builder = Builder::new(&mut raw);
    for (path, body) in entries {
      let mut header = Header::new_gnu();
      header.set_size(body.len() as u64);
      header.set_mode(0o644);
      header.set_cksum();
      builder
        .append_data(&mut header, path, *body)
        .expect("append entry");
    }
    builder.finish().expect("finish tar");
  }
  let mut gz = Vec::new();
  {
    let mut enc = GzEncoder::new(&mut gz, Compression::default());
    enc.write_all(&raw).expect("gzip write");
    enc.finish().expect("gzip finish");
  }
  gz
}

/// Serve `body` (200 OK) for every request on a random localhost port,
/// counting arrivals into `counter`. The server task is aborted when the
/// caller's Tokio runtime shuts down at test end.
async fn spawn_counting_tarball_server(body: Vec<u8>, counter: Arc<AtomicUsize>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    let counter = Arc::clone(&counter);
    async move {
      counter.fetch_add(1, Ordering::SeqCst);
      body
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// Serve `body` (200 OK) for every request, counting arrivals into `counter`,
/// announcing each arrival on `arrivals`, and holding every response open
/// until `release` flips to `true`.
///
/// The hold is what makes a single-flight assertion meaningful: with the
/// first fetch parked mid-response, a second `ensure` call for the same key
/// CANNOT be served by an already-populated cache, so a fetch count of 1 can
/// only mean the second caller joined the in-flight attempt.
async fn spawn_held_counting_tarball_server(
  body: Vec<u8>,
  counter: Arc<AtomicUsize>,
  arrivals: tokio::sync::mpsc::Sender<()>,
  release: tokio::sync::watch::Receiver<bool>,
) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    let counter = Arc::clone(&counter);
    let arrivals = arrivals.clone();
    let mut release = release.clone();
    async move {
      counter.fetch_add(1, Ordering::SeqCst);
      let _ = arrivals.send(()).await;
      loop {
        if *release.borrow() {
          break;
        }
        if release.changed().await.is_err() {
          break;
        }
      }
      body
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// Serve `body` (200 OK) for every request, but hold each response behind a
/// `gate` (sized to the number of concurrent callers under test) — a
/// response is released only once ALL gated requests have arrived, proving
/// genuine overlap rather than a lucky interleave.
async fn spawn_gated_tarball_server(body: Vec<u8>, gate: Arc<tokio::sync::Barrier>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    let gate = Arc::clone(&gate);
    async move {
      gate.wait().await;
      body
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// Fail the first request with a 500, then serve `body` (200 OK) for every
/// request after.
async fn spawn_failing_then_ok_server(body: Vec<u8>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let attempt = Arc::new(AtomicUsize::new(0));
  let app = axum::Router::new().fallback(move || {
    let body = body.clone();
    let attempt = Arc::clone(&attempt);
    async move {
      if attempt.fetch_add(1, Ordering::SeqCst) == 0 {
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Vec::new())
      } else {
        (axum::http::StatusCode::OK, body)
      }
    }
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// Fail every request with a 500.
async fn spawn_always_failing_server() -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind local tarball server");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().fallback(|| async {
    (
      axum::http::StatusCode::INTERNAL_SERVER_ERROR,
      Vec::<u8>::new(),
    )
  });
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  addr
}

/// AC-10a: two concurrent `ensure` calls for the SAME ref must result in
/// exactly ONE tarball fetch, and both callers must get the same `PathBuf`.
///
/// The sequencing is what makes the count assertion mean "single-flight
/// dedupe" rather than the far weaker "the second call was a cache hit": the
/// server parks the first fetch mid-response and only releases it once the
/// second caller is inside `ensure`, so the second call is forced to resolve
/// against an attempt that is still IN FLIGHT. A regression to
/// fetch-per-caller shows up as a second arrival at the (counting) server —
/// which is likewise held, so the test still finishes and fails on the count
/// instead of hanging.
#[tokio::test]
async fn ensure_dedupes_concurrent_prefetch_calls_for_the_same_ref() {
  let tar = build_tarball(&[("acme-repo-v1-abc/README.md", b"same-ref")]);
  let count = Arc::new(AtomicUsize::new(0));
  let (arrival_tx, mut arrival_rx) = tokio::sync::mpsc::channel::<()>(4);
  let (release_tx, release_rx) = tokio::sync::watch::channel(false);
  let addr =
    spawn_held_counting_tarball_server(tar, Arc::clone(&count), arrival_tx, release_rx).await;
  let client = reqwest::Client::new();
  let fetcher = Arc::new(ActionFetcher::new());
  let dest = tmp_dest("dedupe");
  let url = format!("http://{addr}/tarball.tar.gz");

  let first = spawn_ensure(&fetcher, &client, &url, &dest, None);
  // The first fetch has reached the server and is parked there: the
  // single-flight attempt is now unambiguously in flight.
  arrival_rx
    .recv()
    .await
    .expect("the first fetch must reach the server");

  let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
  let second = spawn_ensure(&fetcher, &client, &url, &dest, Some(entered_tx));
  entered_rx
    .await
    .expect("the second caller must enter ensure");

  release_tx.send(true).expect("release the held response");

  let (a, b) = tokio::time::timeout(Duration::from_secs(10), async {
    tokio::join!(first, second)
  })
  .await
  .expect("both ensure calls must settle once the response is released");

  let a = a
    .expect("first ensure task must not panic")
    .expect("first concurrent ensure call must succeed");
  let b = b
    .expect("second ensure task must not panic")
    .expect("second concurrent ensure call must succeed");
  assert_eq!(
    a, b,
    "concurrent callers for the same ref must get the same path"
  );
  assert_eq!(
    count.load(Ordering::SeqCst),
    1,
    "two concurrent callers for the same ref must produce exactly one fetch"
  );
}

/// Spawn one `ensure` call for `acme/repo/v1` as its own task, signalling on
/// `entered` (if given) the moment the task starts — the task then runs
/// straight into `ensure`'s first await, i.e. the single-flight cell.
fn spawn_ensure(
  fetcher: &Arc<ActionFetcher>,
  client: &reqwest::Client,
  url: &str,
  dest: &Path,
  entered: Option<tokio::sync::oneshot::Sender<()>>,
) -> tokio::task::JoinHandle<Result<PathBuf, RunnerError>> {
  let fetcher = Arc::clone(fetcher);
  let client = client.clone();
  let url = url.to_owned();
  let dest = dest.to_path_buf();
  tokio::spawn(async move {
    if let Some(entered) = entered {
      let _ = entered.send(());
    }
    fetcher.ensure(&client, "acme/repo/v1", &url, &dest).await
  })
}

/// AC-10b: two concurrent `ensure` calls for DISTINCT refs must overlap in
/// flight (proved by a 2-party gate that only opens once both arrive)
/// instead of running serially.
#[tokio::test]
async fn ensure_prefetches_distinct_refs_concurrently() {
  let tar = build_tarball(&[("acme-repo-v1-abc/README.md", b"distinct-ref")]);
  let gate = Arc::new(tokio::sync::Barrier::new(2));
  let addr = spawn_gated_tarball_server(tar, Arc::clone(&gate)).await;
  let client = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let dest_a = tmp_dest("distinct-a");
  let dest_b = tmp_dest("distinct-b");
  let url_a = format!("http://{addr}/repos/acme/repo-a/tarball/v1");
  let url_b = format!("http://{addr}/repos/acme/repo-b/tarball/v1");

  // A regression to serial fetching would deadlock here forever (the second
  // request never arrives to open the barrier), so this is bounded by a
  // timeout instead of a bare join.
  let (a, b) = tokio::time::timeout(Duration::from_secs(5), async {
    tokio::join!(
      fetcher.ensure(&client, "acme/repo-a/v1", &url_a, &dest_a),
      fetcher.ensure(&client, "acme/repo-b/v1", &url_b, &dest_b),
    )
  })
  .await
  .expect("both distinct-ref fetches must overlap within the timeout");

  a.expect("ref a must download");
  b.expect("ref b must download");
}

/// AC-10c: a failed fetch evicts its single-flight entry, so a later
/// (sequential) `ensure` call for the same key retries fresh and succeeds.
#[tokio::test]
async fn ensure_retries_a_prefetch_after_the_first_fetch_fails() {
  let tar = build_tarball(&[("acme-repo-v1-abc/README.md", b"retry-me")]);
  let addr = spawn_failing_then_ok_server(tar).await;
  let client = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let dest = tmp_dest("retry");
  let url = format!("http://{addr}/tarball.tar.gz");

  let first = fetcher.ensure(&client, "acme/repo/v1", &url, &dest).await;
  assert!(first.is_err(), "the first fetch (server 500) must fail");

  let second = fetcher.ensure(&client, "acme/repo/v1", &url, &dest).await;
  second.expect("the second ensure call must retry fresh and succeed");
  assert!(
    is_action_cached(&dest),
    "the retried download must have completed and written the watermark"
  );
}

/// `prefetch_refs` reuses `resolve_action_refs` for dedupe: two identical
/// `uses:` strings for the same ref must still produce exactly one fetch.
#[tokio::test]
async fn prefetch_refs_dedupes_duplicate_uses_strings_via_resolve_action_refs() {
  let tar = build_tarball(&[("acme-repo-v1-abc/README.md", b"batch-dedupe")]);
  let count = Arc::new(AtomicUsize::new(0));
  let addr = spawn_counting_tarball_server(tar, Arc::clone(&count)).await;
  let client = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let data_dir = tmp_dest("refs-dedupe-datadir");
  let api_base = format!("http://{addr}");
  let uses_refs = vec!["acme/repo@v1".to_owned(), "acme/repo@v1".to_owned()];

  prefetch_refs(&fetcher, &client, &data_dir, &api_base, &uses_refs).await;

  let cache_dir = action_cache_dir(&data_dir, "acme/repo/v1");
  assert!(
    is_action_cached(&cache_dir),
    "prefetch_refs must have fetched the deduped ref"
  );
  assert_eq!(
    count.load(Ordering::SeqCst),
    1,
    "two identical uses: strings must dedupe to one fetch"
  );
}

/// A prefetch failure is WARN-logged and swallowed by `prefetch_refs` — it
/// must return normally (no panic, no propagated error) and must not leave
/// a fake cache hit behind.
#[tokio::test]
async fn prefetch_refs_swallows_a_failed_fetch_without_failing_the_job() {
  let addr = spawn_always_failing_server().await;
  let client = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let data_dir = tmp_dest("refs-fail-datadir");
  let api_base = format!("http://{addr}");
  let uses_refs = vec!["acme/broken@v1".to_owned()];

  prefetch_refs(&fetcher, &client, &data_dir, &api_base, &uses_refs).await;

  let cache_dir = action_cache_dir(&data_dir, "acme/broken/v1");
  assert!(
    !is_action_cached(&cache_dir),
    "a failed prefetch must not fake a cache hit"
  );
}

/// `top_level_uses_refs` (the sync ref-collection step `spawn_prefetch` runs
/// before spawning) keeps every non-`run:` step's `uses:` string, local and
/// remote alike, and drops `run:` steps entirely.
#[test]
fn top_level_uses_refs_selects_action_steps_for_prefetch() {
  let run_step = ActionStep::script("run1", "echo hi", "");

  let mut remote_step = ActionStep::with_ref_type("uses1", "node20");
  remote_step.reference.name = Some("acme/repo".to_owned());
  remote_step.reference.git_ref = Some("v1".to_owned());

  let mut local_step = ActionStep::with_ref_type("uses2", "node20");
  local_step.reference.name = Some("./local-action".to_owned());

  let refs = top_level_uses_refs(&[run_step, remote_step, local_step]);

  assert_eq!(
    refs,
    vec!["acme/repo@v1".to_owned(), "./local-action".to_owned()]
  );
}

/// `spawn_prefetch` returns a `JoinHandle` that completes on its own for a
/// job with no `uses:` steps — no network is attempted (an empty ref list
/// short-circuits before `resolve_action_refs` builds any URL), and the
/// handle is what the caller (`job_runner::run_job`) aborts at job end.
#[tokio::test]
async fn spawn_prefetch_joinhandle_completes_with_no_uses_steps() {
  let steps = vec![ActionStep::script("s1", "echo hi", "")];
  let fetcher = Arc::new(ActionFetcher::new());
  let client = reqwest::Client::new();
  let data_dir = tmp_dest("spawn-empty-datadir");

  let handle = spawn_prefetch(&steps, fetcher, client, data_dir);

  tokio::time::timeout(Duration::from_secs(5), handle)
    .await
    .expect("prefetch task must not hang")
    .expect("prefetch task must not panic");
}
