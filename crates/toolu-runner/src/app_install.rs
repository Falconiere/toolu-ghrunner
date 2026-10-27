//! Waiting for a GitHub App's installation (#202, design screen 05's second
//! browser trip): `create-app` prints the install link, polls
//! `GET /app/installations` with a fresh App JWT until the App is installed
//! on the target account, and reports the installation. [`installation_on`]
//! is the one "saved App + its installation on this account" lookup: the
//! wait loop and `--import` ask it, and the onboarding flow planned in #204
//! is meant to reuse it rather than add another.
//!
//! On an interactive terminal the user may type `o` (open the link again) or
//! `c` (copy it, OSC 52) followed by Enter while waiting; line-buffered, so
//! no raw terminal mode is needed. After the wait timeout an interactive run
//! asks Keep waiting / Open the link again / Cancel; an unattended one gives
//! up. Ctrl-C is the caller's (`create_app_cmd`).

use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use config::app_store::StoredApp;
use protocol::app_jwt::Installation;
use shared::{RunnerError, SecretMasker};
use tokio::io::{AsyncBufRead, Lines};
use tokio::time::Instant;
use wire::net::github_app::{
  create_installation_token, installation_repo_count, list_installations,
};

use crate::app_auth::{app_jwt, mask, masked};

/// Per-request timeout of the wait's App API calls.
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// The installation of `app` on `account` (ASCII case-insensitive login), if
/// any: `GET <api>/app/installations` with a fresh, masked App JWT.
///
/// # Errors
///
/// As [`list_installations`]: `Network` for transport / 429 / 5xx (worth
/// retrying), `Auth` when GitHub rejects the App or its key; `Config` when
/// the key does not sign.
pub(crate) async fn installation_on(
  client: &reqwest::Client,
  api_base: &str,
  app: &StoredApp,
  account: &str,
  masker: &Mutex<SecretMasker>,
) -> Result<Option<Installation>, RunnerError> {
  let jwt = app_jwt(app, masker)?;
  let installations = list_installations(client, api_base, &jwt, API_TIMEOUT).await?;
  Ok(installations.into_iter().find(|installation| {
    installation
      .account
      .as_ref()
      .is_some_and(|owner| owner.login.eq_ignore_ascii_case(account))
  }))
}

/// What the wait polls for and how it reaches GitHub.
pub(crate) struct WaitTarget<'a> {
  /// The HTTP client for the App API.
  pub(crate) client: &'a reqwest::Client,
  /// The REST API base (`https://api.github.com`).
  pub(crate) api_base: &'a str,
  /// The App being installed.
  pub(crate) app: &'a StoredApp,
  /// The account the installation must belong to.
  pub(crate) account: &'a str,
  /// The masker every JWT and token is registered with.
  pub(crate) masker: &'a Mutex<SecretMasker>,
}

/// How long to wait, and how often to poll.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WaitTiming {
  /// Wait this long before asking (interactive) or giving up (unattended).
  pub(crate) timeout: Duration,
  /// Poll this often.
  pub(crate) poll: Duration,
}

/// How the wait ended.
#[derive(Debug)]
pub(crate) enum WaitOutcome {
  /// The App is installed on the target account.
  Installed(Installation),
  /// The user chose Cancel at the timeout prompt (or input closed).
  Cancelled,
  /// Unattended and the timeout passed.
  TimedOut,
}

/// The choice at the timeout prompt.
#[derive(Debug, PartialEq, Eq)]
enum TimeoutChoice {
  Keep,
  Open,
  Cancel,
}

/// The wait's interaction with the person: the install link, how to open
/// it, the input lines (`None` when unattended) and where to write.
pub(crate) struct WaitIo<'a, R, W> {
  /// The install link.
  pub(crate) url: &'a str,
  /// Opens the link; `false` when there is no browser to launch
  /// (`--no-browser`, a headless session).
  pub(crate) open: &'a dyn Fn(&str) -> bool,
  /// Input lines; `None` when stdin is not a terminal.
  pub(crate) input: Option<Lines<R>>,
  /// Where prompts and status lines go.
  pub(crate) out: &'a mut W,
}

/// Poll until the App is installed on the target account, honoring the line
/// commands and the timeout prompt.
///
/// # Errors
///
/// Any non-`Network` error from [`installation_on`] (the App or its key was
/// rejected), or `Io` when writing to `out` fails. Transient failures are
/// written as a warning and the wait keeps polling.
pub(crate) async fn wait_for_installation<R, W>(
  target: &WaitTarget<'_>,
  timing: WaitTiming,
  io: WaitIo<'_, R, W>,
) -> Result<WaitOutcome, RunnerError>
where
  R: AsyncBufRead + Unpin,
  W: Write,
{
  let WaitIo {
    url,
    open,
    mut input,
    out,
  } = io;
  let interactive = input.is_some();
  writeln!(
    out,
    "Waiting for the installation\u{2026}{}",
    hint(interactive)
  )?;
  let mut deadline = Instant::now() + timing.timeout;
  // Poll on the schedule only: a typed line must not trigger an API call.
  let mut due = true;
  loop {
    if due && let Some(installation) = poll_once(target, out).await? {
      return Ok(WaitOutcome::Installed(installation));
    }
    if Instant::now() >= deadline {
      if let Some(outcome) =
        at_deadline(target.account, interactive, &mut input, url, open, out).await?
      {
        return Ok(outcome);
      }
      deadline = Instant::now() + timing.timeout;
      due = true;
      continue;
    }
    let pause = timing.poll.min(deadline - Instant::now());
    due = pause_or_line(pause, &mut input, url, open, out).await?;
  }
}

/// Sleep for `pause` unless a line arrives first; act on the line. `true`
/// when the pause elapsed (time to poll), `false` after a line (no extra
/// API call).
async fn pause_or_line<R, W>(
  pause: Duration,
  input: &mut Option<Lines<R>>,
  url: &str,
  open: &dyn Fn(&str) -> bool,
  out: &mut W,
) -> Result<bool, RunnerError>
where
  R: AsyncBufRead + Unpin,
  W: Write,
{
  let sleep = tokio::time::sleep(pause);
  let Some(lines) = input.as_mut() else {
    sleep.await;
    return Ok(true);
  };
  tokio::select! {
    () = sleep => Ok(true),
    line = lines.next_line() => {
      match line? {
        Some(line) => line_command(line.trim(), url, open, out)?,
        // Input closed: stop reading; the timeout then cancels.
        None => *input = None,
      }
      Ok(false)
    },
  }
}

/// One poll: the installation if present; a transient failure is written as
/// a warning and reads as "not yet".
async fn poll_once<W: Write>(
  target: &WaitTarget<'_>,
  out: &mut W,
) -> Result<Option<Installation>, RunnerError> {
  let found = installation_on(
    target.client,
    target.api_base,
    target.app,
    target.account,
    target.masker,
  )
  .await;
  match found {
    Err(RunnerError::Network(msg)) => {
      let msg = masked(target.masker, &msg);
      writeln!(out, "warning: {msg}; still waiting")?;
      Ok(None)
    },
    other => other,
  }
}

/// The timeout passed: unattended gives up; interactive asks (closed input
/// cancels). `None` means keep waiting (after re-opening the link if asked).
async fn at_deadline<R, W>(
  account: &str,
  interactive: bool,
  input: &mut Option<Lines<R>>,
  url: &str,
  open: &dyn Fn(&str) -> bool,
  out: &mut W,
) -> Result<Option<WaitOutcome>, RunnerError>
where
  R: AsyncBufRead + Unpin,
  W: Write,
{
  let choice = match input.as_mut() {
    Some(lines) => timeout_choice(account, lines, out).await?,
    None if interactive => TimeoutChoice::Cancel,
    None => return Ok(Some(WaitOutcome::TimedOut)),
  };
  match choice {
    TimeoutChoice::Keep => {},
    TimeoutChoice::Open => reopen(url, open, out)?,
    TimeoutChoice::Cancel => return Ok(Some(WaitOutcome::Cancelled)),
  }
  writeln!(out, "Waiting for the installation\u{2026}{}", hint(true))?;
  Ok(None)
}

/// The key hint after "Waiting for the installation…".
fn hint(interactive: bool) -> &'static str {
  if interactive {
    "  (o + Enter: open the link again \u{b7} c + Enter: copy it \u{b7} ctrl+c: cancel)"
  } else {
    "  (ctrl+c: cancel)"
  }
}

/// Act on a line typed while waiting: `o` opens, `c` copies, anything else
/// repeats the hint.
fn line_command<W: Write>(
  line: &str,
  url: &str,
  open: &dyn Fn(&str) -> bool,
  out: &mut W,
) -> std::io::Result<()> {
  match line {
    "o" | "O" => reopen(url, open, out),
    "c" | "C" => {
      write!(out, "{}", osc52_copy(url))?;
      writeln!(out, "Copied the link: {url}")
    },
    _ => writeln!(out, "Still waiting.{}", hint(true)),
  }
}

/// Open the link again and say so.
fn reopen<W: Write>(url: &str, open: &dyn Fn(&str) -> bool, out: &mut W) -> std::io::Result<()> {
  if open(url) {
    writeln!(out, "Opened {url}")
  } else {
    writeln!(
      out,
      "No browser on this machine \u{2014} open it on another one: {url}"
    )
  }
}

/// The OSC 52 escape that asks the terminal to put `text` on the clipboard
/// (works over SSH in terminals that support it).
pub(crate) fn osc52_copy(text: &str) -> String {
  format!(
    "\u{1b}]52;c;{}\u{7}",
    base64::engine::general_purpose::STANDARD.encode(text)
  )
}

/// Ask Keep waiting / Open the link again / Cancel until a valid answer;
/// closed input cancels.
async fn timeout_choice<R, W>(
  account: &str,
  lines: &mut Lines<R>,
  out: &mut W,
) -> Result<TimeoutChoice, RunnerError>
where
  R: AsyncBufRead + Unpin,
  W: Write,
{
  writeln!(out, "The App isn't installed on {account} yet.")?;
  loop {
    write!(
      out,
      "  [k] Keep waiting  [o] Open the link again  [c] Cancel  (default k): "
    )?;
    out.flush()?;
    let Some(line) = lines.next_line().await? else {
      return Ok(TimeoutChoice::Cancel);
    };
    match line.trim() {
      "" | "k" | "K" => return Ok(TimeoutChoice::Keep),
      "o" | "O" => return Ok(TimeoutChoice::Open),
      "c" | "C" | "q" | "Q" => return Ok(TimeoutChoice::Cancel),
      _ => {},
    }
  }
}

/// `Installed · installation <id> · all repositories` (or the selected
/// repository count, read with an installation token; a failed count falls
/// back to "selected repositories").
pub(crate) async fn installed_line(target: &WaitTarget<'_>, installation: &Installation) -> String {
  let scope = match installation.repository_selection.as_deref() {
    Some("selected") => match selected_count(target, installation.id).await {
      Ok(1) => "1 selected repository".to_owned(),
      Ok(count) => format!("{count} selected repositories"),
      Err(e) => {
        tracing::warn!(error = %e, "could not count the installation's repositories");
        "selected repositories".to_owned()
      },
    },
    _ => "all repositories".to_owned(),
  };
  format!(
    "Installed \u{b7} installation {} \u{b7} {scope}",
    installation.id
  )
}

/// Mint an installation token and count the installation's repositories.
async fn selected_count(target: &WaitTarget<'_>, installation_id: i64) -> Result<u64, RunnerError> {
  let jwt = app_jwt(target.app, target.masker)?;
  let token = create_installation_token(
    target.client,
    target.api_base,
    &jwt,
    installation_id,
    API_TIMEOUT,
  )
  .await?;
  mask(target.masker, &token.token);
  installation_repo_count(target.client, target.api_base, &token.token, API_TIMEOUT).await
}

#[cfg(test)]
#[path = "tests/app_install.rs"]
mod tests;
