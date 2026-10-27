//! Creating the runner's GitHub App on a machine with no browser (#205,
//! design screen 06): the box prints a get.toolu.sh link to open on another
//! machine, and GitHub's one-time code comes back through the relay
//! (CodaSignal/toolu.sh#88), bound to a secret only this box holds — or by
//! one paste. Under `--no-relay` this box never contacts the relay (the
//! laptop still loads its static form page): GitHub redirects to a
//! `127.0.0.1` URL that fails to load on the laptop, and the user pastes it.
//!
//! Anyone holding the printed link knows its `state` and could plant a code
//! for an App they own, so the App GitHub returns must carry the requested
//! name (and owner, when known) before anything is saved.

use std::future::Future;
use std::io::{IsTerminal as _, Write};
use std::pin::Pin;
use std::time::Duration;

use config::app_store;
use protocol::app_manifest::{
  self, AppManifest, ConversionResponse, ManifestTarget, Pasted, parse_pasted,
};
use shared::RunnerError;
use tokio::time::Instant;
use wire::net::app_manifest::{RelayClaim, claim_relay_code, convert_manifest_code};

use crate::app_install::osc52_copy;
use crate::cli::CreateAppArgs;
use crate::create_app_cmd::{
  AppContext, TermLines, build_stored_app, register_secrets, wait_and_record,
};
use crate::run_cmd::CodedExit;

/// How long the box waits for the code before giving up.
const CODE_WAIT: Duration = Duration::from_secs(900);

/// Wait between claims while the relay cannot be reached.
const RELAY_RETRY: Duration = Duration::from_secs(5);

/// Exit code of a wait that saw no code in time (epic #199: runtime failure).
const TIMED_OUT_EXIT: i32 = 1;

/// Exit code of a cancelled wait (epic #199).
const CANCELLED_EXIT: i32 = 130;

/// The secret, its `state`, and where the code can come from.
pub(crate) struct CodeSource<'a> {
  /// The HTTP client the relay is claimed with.
  pub(crate) client: &'a reqwest::Client,
  /// The relay origin to claim from; `None` under `--no-relay`.
  pub(crate) relay: Option<&'a str>,
  /// The secret only this box holds (claims carry it; nothing prints it).
  pub(crate) secret: &'a str,
  /// `SHA-256(secret)`: what the link and GitHub's redirect carry.
  pub(crate) state: &'a str,
  /// The printed link (`c` copies it).
  pub(crate) link: &'a str,
}

/// How the code wait ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodeOutcome {
  /// GitHub's one-time manifest code.
  Code(String),
  /// No code before the deadline.
  TimedOut,
}

/// What a headless create refuses before any request: `--no-relay` with no
/// terminal to paste into, or a name GitHub's form would make the user
/// change (the changed App would then be refused after it exists).
///
/// # Errors
///
/// A message naming the fix.
pub(crate) fn preflight(args: &CreateAppArgs, name: &str, has_input: bool) -> Result<(), String> {
  if args.session.no_relay && !has_input {
    return Err(
      "--no-relay needs a terminal to paste the URL into: run create-app from an interactive \
       shell, or drop --no-relay to let the relay bring the code back"
        .to_owned(),
    );
  }
  let len = name.chars().count();
  if len > crate::create_app_cmd::MAX_APP_NAME {
    return Err(format!(
      "the App name \"{name}\" is {len} characters; GitHub allows {}: pass a shorter --name",
      crate::create_app_cmd::MAX_APP_NAME
    ));
  }
  Ok(())
}

/// Headless create: print the link, wait for the code, exchange it, check
/// it is the App asked for, save it at once, then wait for its installation.
/// [`preflight`] has passed.
///
/// # Errors
///
/// Exit 2 for a relay refusal, a closed terminal under `--no-relay`, a
/// spent code or an App that is not the one asked for (nothing saved);
/// [`CodedExit`] 1 when no code arrives in time, 130 on Ctrl-C; the
/// installation wait's own errors after the App is saved.
pub(crate) async fn create_headless(
  args: &CreateAppArgs,
  ctx: &AppContext<'_>,
  target: &ManifestTarget,
  name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
  let mut input = ctx.take_input();
  let no_relay = args.session.no_relay;
  let secret = app_manifest::new_relay_secret();
  add_secret(ctx, &secret);
  let state = app_manifest::relay_state(&secret);
  let relay_base = crate::config::relay_url();
  let redirect = if no_relay {
    format!("http://127.0.0.1:{}/cb", fastrand::u16(49152..=65535))
  } else {
    app_manifest::relay_callback_url(&relay_base)
  };
  let manifest = AppManifest::for_target(name, &redirect, target);
  let link = app_manifest::relay_link(&relay_base, &manifest, target, &state)?;
  let source = CodeSource {
    client: &ctx.client,
    relay: (!no_relay).then_some(relay_base.as_str()),
    secret: &secret,
    state: &state,
    link: &link,
  };
  let code = await_code(&source, target, &mut input, &mut std::io::stdout()).await?;
  add_secret(ctx, &code);
  let resp = exchange(ctx, &code, target).await?;
  let owner = expected_owner(args, target);
  check_returned_app(&resp, name, owner.as_deref()).map_err(|e| ctx.fail(&e))?;
  let app = build_stored_app(resp, args.host.clone());
  app_store::save_app(ctx.home, &app)?;
  register_secrets(ctx, &app);
  println!("App created \u{b7} {} \u{b7} App {}", app.slug, app.app_id);
  println!("key saved to {} (0600)", ctx.dest.display());
  let account = app.owner.clone();
  // Back for the installation wait: the same reader, no line lost.
  *ctx
    .input
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = input;
  wait_and_record(ctx, app, &account, None).await
}

/// Mask `value` in every error printed from here on.
fn add_secret(ctx: &AppContext<'_>, value: &str) {
  ctx
    .masker
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .add_secrets([value.to_owned()]);
}

/// Exchange the code; a spent one names where to delete the App it made.
async fn exchange(
  ctx: &AppContext<'_>,
  code: &str,
  target: &ManifestTarget,
) -> Result<ConversionResponse, Box<dyn std::error::Error>> {
  convert_manifest_code(&ctx.client, &ctx.api_base, code)
    .await
    .map_err(|e| {
      if matches!(e, RunnerError::Auth(_)) {
        ctx.fail(&spent_code_help(target))
      } else {
        ctx.fail(&e)
      }
    })
}

/// Print the waiting screen and wait for the code; Ctrl-C cancels. A
/// timeout for an org names who may create its Apps (GitHub's refusal page
/// never redirects).
async fn await_code(
  source: &CodeSource<'_>,
  target: &ManifestTarget,
  input: &mut Option<TermLines>,
  out: &mut std::io::Stdout,
) -> Result<String, Box<dyn std::error::Error>> {
  let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
  let outcome = tokio::select! {
    outcome = wait_for_code(source, input, out, CODE_WAIT) => outcome?,
    _ = interrupt.recv() => {
      return Err(Box::new(CodedExit::new(
        CANCELLED_EXIT,
        "cancelled \u{2014} no App was exchanged on this machine".to_owned(),
      )));
    },
  };
  match outcome {
    CodeOutcome::Code(code) => Ok(code),
    CodeOutcome::TimedOut => Err(Box::new(CodedExit::new(
      TIMED_OUT_EXIT,
      timed_out_message(source, target),
    ))),
  }
}

/// Why no code came and what to do next.
fn timed_out_message(source: &CodeSource<'_>, target: &ManifestTarget) -> String {
  let paste = if source.relay.is_some() {
    " (p + Enter pastes the code from the callback page; --no-relay skips the relay)"
  } else {
    ""
  };
  let refused = match target {
    ManifestTarget::Org(org) => format!(
      ". If GitHub refused the form: only owners of {org} (and its GitHub App managers) can \
       create an App for it"
    ),
    ManifestTarget::Personal => String::new(),
  };
  format!(
    "no code arrived after {} minutes \u{2014} run create-app again for a fresh link{paste}{refused}",
    CODE_WAIT.as_secs() / 60
  )
}

/// A relay claim in flight: kept across loop turns, never dropped mid-request
/// (the relay hands a code out once).
type InFlight<'a> = Pin<Box<dyn Future<Output = Result<RelayClaim, RunnerError>> + 'a>>;

/// What woke the wait.
enum Wake {
  /// The in-flight claim finished.
  Claim(Result<RelayClaim, RunnerError>),
  /// A line was typed (or the input closed).
  Line(std::io::Result<Option<String>>),
  /// Time to claim again, or the deadline.
  Tick,
}

/// Print the waiting screen, then poll the relay (if any) and read pasted
/// lines until a code for this run arrives or `timeout` passes. A claim in
/// flight never holds up typed input.
///
/// # Errors
///
/// A relay refusal (`Protocol`: a 400 or an unusable code), a terminal
/// closed with no relay (`Config`) and write failures; an unreachable relay
/// only warns and keeps polling.
pub(crate) async fn wait_for_code<W: Write>(
  source: &CodeSource<'_>,
  input: &mut Option<TermLines>,
  out: &mut W,
  timeout: Duration,
) -> Result<CodeOutcome, RunnerError> {
  let blink = std::io::stdout().is_terminal() && !crate::config::no_color();
  waiting_screen(source, input.is_some(), blink, out)?;
  let deadline = Instant::now() + timeout;
  let mut next_claim = Instant::now();
  let mut warned = false;
  let mut inflight: Option<InFlight<'_>> = None;
  loop {
    if Instant::now() >= deadline {
      return Ok(CodeOutcome::TimedOut);
    }
    start_claim(source, &mut inflight, next_claim);
    let wake_at = match (&inflight, source.relay) {
      (None, Some(_)) => next_claim.min(deadline),
      _ => deadline,
    };
    let wake = tokio::select! {
      claim = poll_claim(&mut inflight), if inflight.is_some() => Wake::Claim(claim),
      line = next_line(input), if input.is_some() => Wake::Line(line),
      () = tokio::time::sleep_until(wake_at) => Wake::Tick,
    };
    match wake {
      Wake::Tick => {},
      Wake::Claim(result) => {
        inflight = None;
        match on_claim(result, &mut warned, out)? {
          Ok(code) => return Ok(CodeOutcome::Code(code)),
          Err(wait) => next_claim = Instant::now() + wait,
        }
      },
      Wake::Line(line) => {
        if let Some(code) = on_input(line?, input, source, out)? {
          return Ok(CodeOutcome::Code(code));
        }
      },
    }
  }
}

/// Start a claim when one is due and none is in flight.
fn start_claim<'a>(source: &CodeSource<'a>, inflight: &mut Option<InFlight<'a>>, due: Instant) {
  if let Some(relay) = source.relay
    && inflight.is_none()
    && Instant::now() >= due
  {
    *inflight = Some(Box::pin(claim_relay_code(
      source.client,
      relay,
      source.secret,
    )));
  }
}

/// Await the claim in flight (the branch is enabled only when there is one).
async fn poll_claim(inflight: &mut Option<InFlight<'_>>) -> Result<RelayClaim, RunnerError> {
  match inflight.as_mut() {
    Some(claim) => claim.await,
    None => std::future::pending().await,
  }
}

/// The next typed line (the branch is enabled only while input is open).
async fn next_line(input: &mut Option<TermLines>) -> std::io::Result<Option<String>> {
  match input.as_mut() {
    Some(lines) => lines.next_line().await,
    None => std::future::pending().await,
  }
}

/// Act on a line, or on closed input: with a relay the wait goes on without
/// the paste; under `--no-relay` nothing else can bring a code.
fn on_input<W: Write>(
  line: Option<String>,
  input: &mut Option<TermLines>,
  source: &CodeSource<'_>,
  out: &mut W,
) -> Result<Option<String>, RunnerError> {
  match line {
    Some(line) => Ok(on_line(line.trim(), source, out)?),
    None if source.relay.is_some() => {
      *input = None;
      Ok(None)
    },
    None => Err(RunnerError::Config(
      "the terminal closed before a URL was pasted; run create-app again".to_owned(),
    )),
  }
}

/// One claim's answer: `Ok(code)`, or how long to wait before the next one.
/// An unreachable relay warns once (the paste stays open) and is retried.
fn on_claim<W: Write>(
  result: Result<RelayClaim, RunnerError>,
  warned: &mut bool,
  out: &mut W,
) -> Result<Result<String, Duration>, RunnerError> {
  match result {
    Ok(RelayClaim::Code(code)) => Ok(Ok(code)),
    Ok(RelayClaim::Pending(wait)) => Ok(Err(wait)),
    Err(RunnerError::Network(msg)) => {
      if !*warned {
        writeln!(
          out,
          "warning: can't reach the relay ({msg}); still trying \u{2014} or p + Enter to \
           paste the code from the callback page"
        )?;
        *warned = true;
      }
      Ok(Err(RELAY_RETRY))
    },
    Err(e) => Err(e),
  }
}

/// Screen 06's waiting lines.
fn waiting_screen<W: Write>(
  source: &CodeSource<'_>,
  interactive: bool,
  blink: bool,
  out: &mut W,
) -> std::io::Result<()> {
  writeln!(
    out,
    "This machine has no browser. Open this link on your laptop:"
  )?;
  writeln!(out, "\n  {}\n", source.link)?;
  let dot = if blink {
    "\u{1b}[5m\u{25cf}\u{1b}[25m"
  } else {
    "\u{25cf}"
  };
  let hint = match (interactive, source.relay.is_some()) {
    (false, _) => "",
    (true, true) => "    p paste code \u{b7} c copy link",
    (true, false) => "    p paste URL \u{b7} c copy link",
  };
  writeln!(out, "{dot} Waiting for GitHub\u{2026}{hint}")
}

/// Act on one typed line: `p` asks for the paste, `c` copies the link,
/// anything else is read as the paste. `Some(code)` when it is this run's.
fn on_line<W: Write>(
  line: &str,
  source: &CodeSource<'_>,
  out: &mut W,
) -> std::io::Result<Option<String>> {
  let need_url = source.relay.is_none();
  match line {
    "" => Ok(None),
    "p" | "P" => {
      writeln!(out, "{}", paste_prompt(need_url))?;
      Ok(None)
    },
    "c" | "C" => {
      write!(out, "{}", osc52_copy(source.link))?;
      writeln!(out, "Copied the link: {}", source.link)?;
      Ok(None)
    },
    pasted => match parse_pasted(pasted, source.state, need_url) {
      Pasted::Code(code) => Ok(Some(code)),
      Pasted::WrongState => {
        writeln!(
          out,
          "\u{2715} That URL is from a different setup run. Nothing was exchanged."
        )?;
        writeln!(out, "   Open the link above again and paste the new URL.")?;
        Ok(None)
      },
      Pasted::Invalid => {
        writeln!(out, "That is not it. {}", paste_prompt(need_url))?;
        Ok(None)
      },
    },
  }
}

/// What to paste: the relay's callback page shows the code; under
/// `--no-relay` only the address-bar URL carries the `state` to check.
fn paste_prompt(need_url: bool) -> &'static str {
  if need_url {
    "Paste the address-bar URL of the page that failed to load (127.0.0.1/cb?code=\u{2026}):"
  } else {
    "Paste the code from the callback page:"
  }
}

/// The account the App must belong to: the org, `--account`, or `setup`'s
/// repository owner; `None` when nothing names it.
fn expected_owner(args: &CreateAppArgs, target: &ManifestTarget) -> Option<String> {
  match target {
    ManifestTarget::Org(org) => Some(org.clone()),
    ManifestTarget::Personal => args.account.clone().or_else(|| args.expected_owner.clone()),
  }
}

/// Refuse an App that is not the one this run asked for: a code planted
/// through the printed link (or a rename on GitHub's form) returns another
/// name or owner. Nothing is saved then.
///
/// # Errors
///
/// A message naming what came back, what was asked for, and what to do.
pub(crate) fn check_returned_app(
  resp: &ConversionResponse,
  name: &str,
  owner: Option<&str>,
) -> Result<(), String> {
  let owner_ok = owner.is_none_or(|owner| resp.owner.login.eq_ignore_ascii_case(owner));
  if resp.name == name && owner_ok {
    return Ok(());
  }
  Err(format!(
    "\u{2715} GitHub returned the App \"{}\" owned by {}, but this run asked for \"{name}\"{}. \
     It was not saved. If you renamed it on GitHub's form, delete it at {} and run create-app \
     again with --name <the name you want>; if you did not create it, do not install it.",
    resp.name,
    resp.owner.login,
    owner
      .map(|owner| format!(" owned by {owner}"))
      .unwrap_or_default(),
    resp.html_url
  ))
}

/// A spent or unknown code: an App may exist on GitHub already, so name
/// where to delete it before starting again.
fn spent_code_help(target: &ManifestTarget) -> String {
  let apps = match target {
    ManifestTarget::Personal => "https://github.com/settings/apps".to_owned(),
    ManifestTarget::Org(org) => format!("https://github.com/organizations/{org}/settings/apps"),
  };
  format!(
    "GitHub refused the code: it was already used or has expired. If GitHub created an App, \
     delete it at {apps}, then run create-app again"
  )
}
