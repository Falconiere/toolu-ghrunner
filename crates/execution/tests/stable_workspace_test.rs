//! Stable per-repository workspaces (issue #90, AC-1..AC-5, 90-S1..S4).
//!
//! Replays the sanitized #68 GitHub.com acquisition
//! (`incoming_contexts_matrix_0.json`, its `__run` step) through the
//! production `Runner` → `run_job` path. Only `github.repository` /
//! `repository_owner`, the job id, `workspace.clean` and the step script are
//! rewritten. Each script checks out a real local bare origin with real `git`
//! (clone when the workspace has no `.git`, fetch when it does), so the
//! assertions observe actual `.git` reuse — inode, fetched revision, file
//! content — not just path strings. No mocks.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use execution::Runner;
use shared::{
  AgentJobRequestMessage, Conclusion, PipelineContextData, RunnerConfig, RunnerEvent, SecretMasker,
  TemplateToken, WorkspaceConfig, WorkspaceMode, WorkspaceOptions,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Real git origins
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> TestResult<String> {
  let output = Command::new("git")
    .args([
      "-c",
      "user.name=toolu",
      "-c",
      "user.email=toolu@example.invalid",
    ])
    .args([
      "-c",
      "init.defaultBranch=main",
      "-c",
      "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(dir)
    .output()?;
  if !output.status.success() {
    return Err(
      format!(
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
      )
      .into(),
    );
  }
  Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// A bare origin for `owner/name` with two commits; `id` holds
/// `<owner>/<name>@<n>` so a checkout names its repository and revision.
struct Origin {
  url: String,
  first: String,
  second: String,
}

fn origin(root: &Path, owner: &str, name: &str) -> TestResult<Origin> {
  let bare = root.join("origins").join(owner).join(format!("{name}.git"));
  std::fs::create_dir_all(&bare)?;
  git(&bare, &["init", "-q", "--bare"])?;
  let seed = root.join("seed").join(owner).join(name);
  std::fs::create_dir_all(&seed)?;
  git(&seed, &["init", "-q"])?;
  let mut heads = Vec::new();
  for n in 1..=2 {
    std::fs::write(seed.join("id"), format!("{owner}/{name}@{n}\n"))?;
    git(&seed, &["add", "id"])?;
    git(&seed, &["commit", "-q", "-m", &format!("commit {n}")])?;
    heads.push(git(&seed, &["rev-parse", "HEAD"])?);
  }
  let url = bare.to_string_lossy().into_owned();
  git(&seed, &["push", "-q", &url, "HEAD:refs/heads/main"])?;
  let mut heads = heads.into_iter();
  Ok(Origin {
    url,
    first: heads.next().ok_or("first commit")?,
    second: heads.next().ok_or("second commit")?,
  })
}

/// Clone-or-fetch `rev` of `url` into `dir` (relative to the step's cwd, the
/// workspace) and report whether a `.git` was already there, its inode, and
/// the checked-out revision/content.
fn checkout_script(label: &str, dir: &str, url: &str, rev: &str) -> String {
  format!(
    r#"mkdir -p '{dir}' && cd '{dir}'
if [ -d .git ]; then reused=true; inode=$(ls -di .git | awk '{{print $1}}'); else reused=false; inode=none; git init -q . && git remote add origin '{url}'; fi
git fetch -q origin '{rev}' && git -c advice.detachedHead=false checkout -q --detach FETCH_HEAD
printf 'CO90|{label}|%s|%s|%s|%s|%s\n' "$reused" "$inode" "$(git rev-parse HEAD)" "$(cat id)" "$PWD"
cd "$GITHUB_WORKSPACE""#
  )
}

/// Print the workspace values every job sees.
const PATHS: &str = r#"printf 'WS90|%s|%s|%s|%s\n' "$GITHUB_WORKSPACE" '${{ github.workspace }}' "$RUNNER_WORKSPACE" '${{ runner.workspace }}'"#;

// ---------------------------------------------------------------------------
// Captured-message shaping
// ---------------------------------------------------------------------------

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

/// The captured job for `repository`, reduced to one `run:` step with
/// `body`, a new job id, and `workspace.clean` = `clean`.
fn job(repository: &str, body: &str, clean: Option<&str>) -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job.job_id = uuid::Uuid::new_v4().to_string();
  job.workspace = Some(WorkspaceOptions {
    clean: clean.map(str::to_owned),
  });
  let (owner, _) = repository.split_once('/').ok_or("owner/name")?;
  let github = job
    .context_data
    .get_mut("github")
    .and_then(|github| github.d.as_mut())
    .ok_or("captured github context absent")?;
  for entry in github.iter_mut() {
    match entry.key.s.as_deref() {
      Some("repository") => entry.value = PipelineContextData::string(repository.to_owned()),
      Some("repository_owner") => entry.value = PipelineContextData::string(owner.to_owned()),
      _ => {},
    }
  }
  job
    .steps
    .retain(|step| step.context_name.as_deref() == Some("__run"));
  let step = job.steps.first_mut().ok_or("captured script step absent")?;
  let script = step
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script token absent")?;
  script.value = literal(&format!("set -eu\n{PATHS}\n{body}"));
  Ok(job)
}

// ---------------------------------------------------------------------------
// Production replay
// ---------------------------------------------------------------------------

struct Host {
  root: tempfile::TempDir,
}

impl Host {
  fn new() -> TestResult<Self> {
    Ok(Self {
      root: tempfile::tempdir()?,
    })
  }

  fn path(&self) -> &Path {
    self.root.path()
  }

  fn work(&self) -> PathBuf {
    self.path().join("work")
  }

  /// A registration on the shared work root, identified by its `data_dir`.
  fn config(&self, registration: &str, mode: WorkspaceMode) -> RunnerConfig {
    RunnerConfig {
      data_dir: self.path().join("registrations").join(registration),
      workspace_root: self.work(),
      workspace_gc_hours: 0,
      workspace: WorkspaceConfig {
        mode,
        stable_gc_hours: 0,
      },
      ..RunnerConfig::default()
    }
  }
}

struct Finished {
  lines: Vec<String>,
  conclusion: Option<Conclusion>,
}

impl Finished {
  /// Fields of the first `<prefix>|…` line whose label (first field) is
  /// `label`, or of the first `<prefix>|…` line when `label` is empty.
  fn marker(&self, prefix: &str, label: &str) -> TestResult<Vec<String>> {
    self
      .lines
      .iter()
      .filter_map(|line| line.strip_prefix(prefix)?.strip_prefix('|'))
      .map(|rest| rest.split('|').map(str::to_owned).collect::<Vec<_>>())
      .find(|fields| label.is_empty() || fields.first().map(String::as_str) == Some(label))
      .ok_or_else(|| format!("no {prefix}|{label} marker in {:?}", self.lines).into())
  }

  /// `(GITHUB_WORKSPACE, github.workspace, RUNNER_WORKSPACE, runner.workspace)`.
  fn paths(&self) -> TestResult<[String; 4]> {
    let fields = self.marker("WS90", "")?;
    let fields: [String; 4] = fields
      .try_into()
      .map_err(|fields| format!("WS90 marker shape: {fields:?}"))?;
    Ok(fields)
  }

  /// `GITHUB_WORKSPACE` as the step saw it.
  fn workspace(&self) -> TestResult<String> {
    let [env, ..] = self.paths()?;
    Ok(env)
  }

  /// The trimmed value of a `<prefix>|<label>|<value>` line.
  fn value(&self, prefix: &str, label: &str) -> TestResult<String> {
    let fields = self.marker(prefix, label)?;
    let value = fields.get(1).ok_or("marker without a value")?;
    Ok(value.trim().to_owned())
  }

  /// `(reused, inode, HEAD, id content, PWD)` of checkout `label`.
  fn checkout(&self, label: &str) -> TestResult<Checkout> {
    let fields = self.marker("CO90", label)?;
    let [_, reused, inode, head, id, pwd]: [String; 6] = fields
      .try_into()
      .map_err(|fields| format!("CO90 marker shape: {fields:?}"))?;
    Ok(Checkout {
      reused: reused == "true",
      inode,
      head,
      id,
      pwd,
    })
  }

  fn assert_success(&self) {
    assert_eq!(
      self.conclusion,
      Some(Conclusion::Success),
      "job lines: {:?}",
      self.lines
    );
  }
}

struct Checkout {
  reused: bool,
  inode: String,
  head: String,
  id: String,
  pwd: String,
}

async fn run(config: &RunnerConfig, job: AgentJobRequestMessage) -> TestResult<Finished> {
  let runner = Runner::new(config.clone(), Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(job, CancellationToken::new());
  let mut lines = Vec::new();
  let mut conclusion = None;
  tokio::time::timeout(RUN_TIMEOUT, async {
    while let Some(event) = receiver.recv().await {
      if let RunnerEvent::Log { line, .. } = event {
        lines.push(line);
      } else if let RunnerEvent::JobCompleted {
        conclusion: done, ..
      } = event
      {
        conclusion = Some(done);
      }
    }
  })
  .await
  .map_err(|error| format!("replay timed out: {error}; lines: {lines:?}"))?;
  Ok(Finished { lines, conclusion })
}

fn inode(path: &Path) -> TestResult<u64> {
  use std::os::unix::fs::MetadataExt;
  Ok(std::fs::metadata(path)?.ino())
}

/// A string field of a JSON document by pointer.
fn field<'a>(document: &'a serde_json::Value, pointer: &str) -> Option<&'a str> {
  document
    .pointer(pointer)
    .and_then(serde_json::Value::as_str)
}

fn show(path: &Path) -> String {
  path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// AC-1 / AC-2 — 90-S1 (and the 90-S4 custom-path checkout)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn stable_workspace_reuses_git_across_jobs_and_fresh_mode_stays_isolated() -> TestResult {
  let host = Host::new()?;
  let app = origin(host.path(), "alice", "app")?;
  let lib = origin(host.path(), "alice", "lib")?;
  let stable = host.config("alice-app", WorkspaceMode::Stable);
  let body = |rev: &str| {
    format!(
      "{}\n{}",
      checkout_script("main", ".", &app.url, rev),
      checkout_script("lib", "sub/lib", &lib.url, &lib.first)
    )
  };

  let first = run(&stable, job("alice/app", &body(&app.first), None)?).await?;
  first.assert_success();
  let second = run(&stable, job("alice/app", &body(&app.second), None)?).await?;
  second.assert_success();

  let workspace = host.work().join("app").join("app");
  let pipeline = host.work().join("app");
  for finished in [&first, &second] {
    let [env, expr, runner_env, runner_expr] = finished.paths()?;
    assert_eq!(env, show(&workspace), "GITHUB_WORKSPACE");
    assert_eq!(expr, env, "github.workspace");
    assert_eq!(runner_env, show(&pipeline), "RUNNER_WORKSPACE");
    assert_eq!(runner_expr, runner_env, "runner.workspace");
  }

  // First job clones; the second finds the same .git and only fetches.
  let (one, two) = (first.checkout("main")?, second.checkout("main")?);
  assert!(!one.reused, "first job must clone");
  assert!(two.reused, "second job must reuse the first job's .git");
  let git_inode = inode(&workspace.join(".git"))?.to_string();
  assert_eq!(two.inode, git_inode, "same .git directory");
  assert_eq!(
    (one.head.as_str(), one.id.as_str()),
    (app.first.as_str(), "alice/app@1")
  );
  assert_eq!(
    (two.head.as_str(), two.id.as_str()),
    (app.second.as_str(), "alice/app@2")
  );
  // A second repository checked out to a custom path is reused too.
  assert!(!first.checkout("lib")?.reused);
  let lib_second = second.checkout("lib")?;
  assert!(lib_second.reused, "custom-path checkout reused");
  assert_eq!(lib_second.pwd, show(&workspace.join("sub/lib")));
  assert_eq!(lib_second.id, "alice/lib@1");

  // The tracking file names the repository, its directories and the owner.
  let mapping = host
    .work()
    .join("_PipelineMapping/alice/app/PipelineFolder.json");
  let tracking: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mapping)?)?;
  assert_eq!(field(&tracking, "/repositoryName"), Some("alice/app"));
  assert_eq!(field(&tracking, "/pipelineDirectory"), Some("app"));
  assert_eq!(field(&tracking, "/workspaceDirectory"), Some("app/app"));
  assert_eq!(
    field(&tracking, "/repositories/alice~1app/repositoryPath"),
    Some("app/app")
  );
  assert_eq!(
    field(&tracking, "/dataDir"),
    Some(show(&stable.data_dir).as_str())
  );
  let last_run = tracking
    .pointer("/lastRunOnUnix")
    .and_then(serde_json::Value::as_u64);
  assert!(last_run.is_some_and(|t| t > 0), "{tracking}");

  // AC-2: fresh mode gets a new directory, clones, and leaves the stable one alone.
  let fresh = host.config("alice-app", WorkspaceMode::Fresh);
  let fresh_job = job("alice/app", &body(&app.second), None)?;
  let job_dir = host.work().join(&fresh_job.job_id);
  let third = run(&fresh, fresh_job).await?;
  third.assert_success();
  let [env, expr, runner_env, _] = third.paths()?;
  assert_eq!(env, show(&job_dir));
  assert_eq!(expr, env);
  assert_eq!(
    runner_env,
    show(&job_dir),
    "fresh mode keeps the pre-#90 layout"
  );
  assert!(
    !third.checkout("main")?.reused,
    "fresh workspace has no .git"
  );
  assert_eq!(inode(&workspace.join(".git"))?.to_string(), git_inode);
  assert_eq!(
    git(&workspace, &["rev-parse", "HEAD"])?,
    app.second,
    "stable workspace untouched by the fresh job"
  );
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-3 — 90-S2
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn same_repository_name_under_two_owners_and_other_repos_never_collide() -> TestResult {
  let host = Host::new()?;
  let alice_app = origin(host.path(), "alice", "app")?;
  let bob_app = origin(host.path(), "bob", "app")?;
  let alice_lib = origin(host.path(), "alice", "lib")?;
  let config = host.config("shared", WorkspaceMode::Stable);

  let mut seen = Vec::new();
  for (repository, source, dir) in [
    ("alice/app", &alice_app, "app"),
    ("bob/app", &bob_app, "app_2"),
    ("alice/lib", &alice_lib, "lib"),
  ] {
    let finished = run(
      &config,
      job(
        repository,
        &checkout_script("main", ".", &source.url, &source.first),
        None,
      )?,
    )
    .await?;
    finished.assert_success();
    let name = repository.rsplit('/').next().ok_or("name")?;
    let expected = host.work().join(dir).join(name);
    let [env, ..] = finished.paths()?;
    assert_eq!(env, show(&expected), "{repository}");
    let checkout = finished.checkout("main")?;
    assert!(
      !checkout.reused,
      "{repository} must not see another repo's .git"
    );
    assert_eq!(checkout.head, source.first);
    assert_eq!(checkout.id, format!("{repository}@1"));
    seen.push(env);
  }
  seen.sort();
  seen.dedup();
  assert_eq!(seen.len(), 3, "three distinct workspaces");

  // bob/app keeps its suffixed directory and moves to a new revision there.
  let again = run(
    &config,
    job(
      "bob/app",
      &checkout_script("main", ".", &bob_app.url, &bob_app.second),
      None,
    )?,
  )
  .await?;
  again.assert_success();
  assert_eq!(again.workspace()?, show(&host.work().join("app_2/app")));
  let checkout = again.checkout("main")?;
  assert!(checkout.reused);
  assert_eq!(
    (checkout.head, checkout.id),
    (bob_app.second.clone(), "bob/app@2".to_owned())
  );
  // alice/app's workspace still holds alice's first revision.
  assert_eq!(
    git(&host.work().join("app/app"), &["rev-parse", "HEAD"])?,
    alice_app.first
  );
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-4 — 90-S3
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_job_for_a_leased_repository_runs_in_a_fresh_workspace() -> TestResult {
  let host = Host::new()?;
  let sync = host.path().join("sync");
  std::fs::create_dir_all(&sync)?;
  let (ready, release) = (sync.join("ready"), sync.join("release"));
  // Two registrations (repo-level and org-level) share the default work root.
  let repo_level = host.config("repo-level", WorkspaceMode::Stable);
  let org_level = host.config("org-level", WorkspaceMode::Stable);

  let holder_body = format!(
    "printf a > marker-a\n: > '{}'\nfor _ in $(seq 600); do [ -e '{}' ] && break; sleep 0.1; done\n[ -e '{}' ]\nprintf 'LS90|holder|%s\\n' \"$(ls -A | tr '\\n' ' ')\"",
    show(&ready),
    show(&release),
    show(&release)
  );
  let holder = tokio::spawn({
    let config = repo_level.clone();
    let job = job("alice/app", &holder_body, None)?;
    async move { run(&config, job).await.map_err(|error| error.to_string()) }
  });
  let started = Instant::now();
  while !ready.exists() {
    assert!(
      started.elapsed() < RUN_TIMEOUT && !holder.is_finished(),
      "holder job ended or never reached its workspace"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
  }

  let concurrent_job = job(
    "alice/app",
    "printf b > marker-b\nprintf 'LS90|concurrent|%s\\n' \"$(ls -A | tr '\\n' ' ')\"",
    None,
  )?;
  let concurrent_dir = host.work().join(&concurrent_job.job_id);
  let concurrent = run(&org_level, concurrent_job).await?;
  concurrent.assert_success();
  std::fs::write(&release, b"")?;
  let holder = holder
    .await?
    .map_err(|error| -> Box<dyn Error> { error.into() })?;
  holder.assert_success();

  let stable = host.work().join("app").join("app");
  assert_eq!(holder.workspace()?, show(&stable));
  assert_eq!(concurrent.workspace()?, show(&concurrent_dir));
  assert_eq!(concurrent.value("LS90", "concurrent")?, "marker-b");
  assert_eq!(holder.value("LS90", "holder")?, "marker-a");
  assert!(!stable.join("marker-b").exists());
  assert!(!concurrent_dir.join("marker-a").exists());

  // Once the holder is done the stable workspace is free again.
  let after = run(
    &org_level,
    job(
      "alice/app",
      "printf 'LS90|after|%s\\n' \"$(ls -A | tr '\\n' ' ')\"",
      None,
    )?,
  )
  .await?;
  after.assert_success();
  assert_eq!(after.workspace()?, show(&stable));
  assert_eq!(after.value("LS90", "after")?, "marker-a");
  Ok(())
}

// ---------------------------------------------------------------------------
// AC-5 — 90-S4 workspace.clean
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn workspace_clean_options_follow_upstream() -> TestResult {
  let observe = r#"git_dir=no; [ -d .git ] && git_dir=yes
sibling=no; [ -e "$RUNNER_WORKSPACE/sibling/keep" ] && sibling=yes
printf 'CL90|state|%s|%s\n' "$git_dir" "$sibling""#;
  // (clean option, .git kept, pipeline sibling kept)
  let cases = [
    (None, "yes", "yes"),
    (Some("true"), "yes", "yes"),
    (Some("all"), "no", "no"),
    (Some("resources"), "no", "yes"),
    (Some("outputs"), "yes", "no"),
    (Some("OUTPUTS"), "yes", "no"),
  ];
  for (clean, git_kept, sibling_kept) in cases {
    let host = Host::new()?;
    let app = origin(host.path(), "alice", "app")?;
    let config = host.config("alice-app", WorkspaceMode::Stable);
    let populate = run(
      &config,
      job(
        "alice/app",
        &format!(
          "{}\nmkdir -p \"$RUNNER_WORKSPACE/sibling\" && : > \"$RUNNER_WORKSPACE/sibling/keep\"",
          checkout_script("main", ".", &app.url, &app.first)
        ),
        None,
      )?,
    )
    .await?;
    populate.assert_success();
    let cleaned = run(&config, job("alice/app", observe, clean)?).await?;
    cleaned.assert_success();
    let [_, git_dir, sibling]: [String; 3] = cleaned
      .marker("CL90", "state")?
      .try_into()
      .map_err(|fields| format!("CL90 marker shape: {fields:?}"))?;
    assert_eq!(
      (git_dir.as_str(), sibling.as_str()),
      (git_kept, sibling_kept),
      "clean = {clean:?}"
    );
    assert_eq!(
      cleaned.workspace()?,
      show(&host.work().join("app/app")),
      "clean = {clean:?} keeps the stable path"
    );
  }
  Ok(())
}
