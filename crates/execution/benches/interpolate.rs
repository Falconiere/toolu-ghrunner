#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Criterion bench for the T2 fast paths
//! (`docs/toolu/specs/2026-08-05-job-wallclock-tier1-design.md`).
//!
//! Builds a real `ExecutionContext` via the engine's own
//! `job_runner::build_context`, from a copy of the real `job_message.json`
//! fixture (`benches/fixtures/`, copied rather than `include_str!`-ed
//! across crates). That fixture's `contextData.github` is 15 flat string
//! keys with no `event` object, so a realistic `pull_request` event
//! payload is attached on top of the built context — otherwise this bench
//! never exercises the deep-clone T2a/T2b exist to avoid.
//!
//! Three cases, each at a small and a large `github.event` size, to show
//! the no-`${{` and `literal_status_condition` paths stay flat as the
//! event payload grows while the `${{`-bearing path scales with it:
//! - a literal string with no `${{`
//! - `"${{ github.repository }}"`
//! - the default `if: success()` condition

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use execution::execution::context::ExecutionContext;
use execution::execution::job_runner::build_context;
use expressions::types::ExprValue;
use shared::{AgentJobRequestMessage, RunnerConfig, SecretMasker};

/// Real job message, copied from
/// `crates/toolu-runner/tests/fixtures/job_message.json`.
const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

/// `pull_request.body` length for the small/large `github.event` variants.
const SMALL_BODY_LEN: usize = 120;
const LARGE_BODY_LEN: usize = 6_000;

/// Build a real `ExecutionContext` from the fixture message plus a
/// `pull_request` event payload padded to `body_len` bytes.
fn build_ctx(data_dir: &std::path::Path, body_len: usize) -> ExecutionContext {
  let msg: AgentJobRequestMessage = serde_json::from_str(JOB_MESSAGE).expect("valid fixture");
  let config = RunnerConfig {
    data_dir: data_dir.to_path_buf(),
    workspace_root: data_dir.join("_work"),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = build_context(&msg, &config, masker);
  ctx.set_github_context_value("event", pull_request_event(body_len));
  ctx
}

/// A realistic `pull_request` event body (number/title/body/user/base/head/
/// labels), with `body` padded to `body_len` bytes to control payload size.
fn pull_request_event(body_len: usize) -> ExprValue {
  let mut pr = HashMap::new();
  pr.insert("number".to_owned(), ExprValue::Number(42.0));
  pr.insert(
    "title".to_owned(),
    ExprValue::String("Add the accelerated cache mode".to_owned()),
  );
  pr.insert("body".to_owned(), ExprValue::String("x".repeat(body_len)));
  pr.insert("state".to_owned(), ExprValue::String("open".to_owned()));
  pr.insert("draft".to_owned(), ExprValue::Bool(false));
  pr.insert("user".to_owned(), ExprValue::Object(actor("octocat", 1.0)));
  pr.insert(
    "base".to_owned(),
    ExprValue::Object(branch("main", "d6cd1e2bd19e03a81132a23b2025920577f84e37")),
  );
  pr.insert(
    "head".to_owned(),
    ExprValue::Object(branch(
      "feature/cache",
      "a1b2c3d4e5f60718293a4b5c6d7e8f9091a2b3c4",
    )),
  );
  pr.insert("labels".to_owned(), ExprValue::Array(labels()));

  let mut event = HashMap::new();
  event.insert("action".to_owned(), ExprValue::String("opened".to_owned()));
  event.insert("number".to_owned(), ExprValue::Number(42.0));
  event.insert("pull_request".to_owned(), ExprValue::Object(pr));
  ExprValue::Object(event)
}

fn actor(login: &str, id: f64) -> HashMap<String, ExprValue> {
  let mut user = HashMap::new();
  user.insert("login".to_owned(), ExprValue::String(login.to_owned()));
  user.insert("id".to_owned(), ExprValue::Number(id));
  user
}

fn branch(name: &str, sha: &str) -> HashMap<String, ExprValue> {
  let mut b = HashMap::new();
  b.insert("ref".to_owned(), ExprValue::String(name.to_owned()));
  b.insert("sha".to_owned(), ExprValue::String(sha.to_owned()));
  b
}

fn labels() -> Vec<ExprValue> {
  ["performance", "cache", "needs-review"]
    .iter()
    .map(|name| {
      let mut label = HashMap::new();
      label.insert("name".to_owned(), ExprValue::String((*name).to_owned()));
      ExprValue::Object(label)
    })
    .collect()
}

/// The two `github.event` size variants every bench group runs over.
const SIZES: [(&str, usize); 2] = [
  ("small_event", SMALL_BODY_LEN),
  ("large_event", LARGE_BODY_LEN),
];

fn bench_interpolate_literal(c: &mut Criterion) {
  let dir = tempfile::tempdir().expect("tempdir");
  let mut group = c.benchmark_group("interpolate_no_expression");
  for (label, body_len) in SIZES {
    let ctx = build_ctx(dir.path(), body_len);
    group.bench_with_input(BenchmarkId::from_parameter(label), &ctx, |b, ctx| {
      b.iter(|| {
        black_box(
          ctx
            .interpolate_string(black_box("just a plain literal string, no expression"))
            .unwrap(),
        )
      });
    });
  }
  group.finish();
}

fn bench_interpolate_expression(c: &mut Criterion) {
  let dir = tempfile::tempdir().expect("tempdir");
  let mut group = c.benchmark_group("interpolate_github_repository");
  for (label, body_len) in SIZES {
    let ctx = build_ctx(dir.path(), body_len);
    group.bench_with_input(BenchmarkId::from_parameter(label), &ctx, |b, ctx| {
      b.iter(|| {
        black_box(
          ctx
            .interpolate_string(black_box("${{ github.repository }}"))
            .unwrap(),
        )
      });
    });
  }
  group.finish();
}

fn bench_default_condition(c: &mut Criterion) {
  let dir = tempfile::tempdir().expect("tempdir");
  let mut group = c.benchmark_group("literal_status_condition_success");
  for (label, body_len) in SIZES {
    let ctx = build_ctx(dir.path(), body_len);
    group.bench_with_input(BenchmarkId::from_parameter(label), &ctx, |b, ctx| {
      b.iter(|| black_box(ctx.literal_status_condition(black_box("success()"))));
    });
  }
  group.finish();
}

criterion_group!(
  benches,
  bench_interpolate_literal,
  bench_interpolate_expression,
  bench_default_condition
);
criterion_main!(benches);
