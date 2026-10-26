//! The device-code side of `setup`'s toolu.sh branch (#207, screen 16):
//! what the box reports about itself, and the wait for a console approval —
//! polling `POST /runner/pair/token` no faster than `interval` (+5 s per
//! `slow_down`, never less), a `● Waiting for approval… expires in MM:SS`
//! line rewritten once a second on a terminal, and the code's expiry.

use std::io::Write as _;
use std::time::{Duration, Instant};

use protocol::toolu_pair::{HostGrant, MachineReport, PairPoll, PairStart};
use wire::net::toolu_jobs::TooluError;
use wire::net::toolu_pair::pair_token;

use super::plan::SetupError;
use super::prompts::{Key, KeySource, PromptEnd};
use super::render::Out;

/// What `slow_down` adds to the poll interval (the contract's five seconds).
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// How a pairing wait ended.
#[derive(Debug)]
pub(crate) enum Waited {
  /// Approved: the machine and its token.
  Granted(HostGrant),
  /// An org owner or admin denied it.
  Denied,
  /// The code expired (or toolu.sh no longer knows it).
  Expired,
}

/// What this box reports to toolu.sh: hostname, `linux`/`macos`,
/// `x64`/`arm64`, CPUs, memory, the custom `labels` and the toolu-runner
/// version.
pub(crate) fn machine_report(
  labels: &[String],
  cpus: usize,
  memory_mb: Option<u64>,
) -> MachineReport {
  MachineReport {
    hostname: hostname::get().map_or_else(
      |_| "unknown".to_owned(),
      |name| name.to_string_lossy().into_owned(),
    ),
    // Rust's `OS` is already toolu.sh's spelling: `linux`, `macos`.
    platform: std::env::consts::OS.to_owned(),
    arch: arch(std::env::consts::ARCH).to_owned(),
    vcpu: u64::try_from(cpus).ok(),
    memory_mb,
    labels: labels.to_vec(),
    runner_version: env!("CARGO_PKG_VERSION").to_owned(),
  }
}

/// toolu.sh's architecture spelling of a Rust `ARCH`; others pass through
/// for toolu.sh to refuse by name.
pub(crate) fn arch(rust_arch: &str) -> &str {
  match rust_arch {
    "x86_64" => "x64",
    "aarch64" => "arm64",
    other => other,
  }
}

/// The interval after a poll answered `poll`: `slow_down` adds five
/// seconds for every later poll; nothing ever shortens it.
pub(crate) fn next_interval(current: Duration, poll: &PairPoll) -> Duration {
  match poll {
    PairPoll::SlowDown => current.saturating_add(SLOW_DOWN_STEP),
    PairPoll::Granted(_)
    | PairPoll::Pending
    | PairPoll::Denied
    | PairPoll::Expired
    | PairPoll::Other(_) => current,
  }
}

/// `MM:SS` of `left`, rounded up to the second.
pub(crate) fn countdown(left: Duration) -> String {
  let secs = left.as_millis().div_ceil(1000);
  format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// `host/path` of a verification URI, the way screen 16 shows it.
pub(crate) fn display_uri(uri: &str) -> &str {
  uri
    .strip_prefix("https://")
    .or_else(|| uri.strip_prefix("http://"))
    .unwrap_or(uri)
}

/// Poll `start`'s device code until it is approved, denied or expired. A
/// transient failure (429, 5xx, no answer) is retried at the current
/// interval: a blip must not lose a code someone is approving.
///
/// # Errors
///
/// A toolu.sh answer outside the contract.
pub(crate) async fn wait_for_approval(
  client: &reqwest::Client,
  api_url: &str,
  start: &PairStart,
  out: Out,
) -> Result<Waited, SetupError> {
  let deadline = Instant::now() + Duration::from_secs(start.expires_in);
  let mut interval = Duration::from_secs(start.interval.max(1));
  if out.plain {
    out.waiting(
      "pairing",
      &format!(
        "Waiting for approval… (the code expires in {})",
        countdown(deadline.saturating_duration_since(Instant::now()))
      ),
    );
  }
  loop {
    tick_until(Instant::now() + interval, deadline, out).await;
    if Instant::now() >= deadline {
      clear_line(out);
      return Ok(Waited::Expired);
    }
    let poll = match pair_token(client, api_url, &start.device_code).await {
      Ok(poll) => poll,
      Err(TooluError::Transient(why)) => {
        tracing::warn!(%why, "toolu.sh did not answer a pairing poll; retrying");
        continue;
      },
      Err(e) => {
        clear_line(out);
        return Err(SetupError::runtime(format!(
          "polling the pairing failed: {e}"
        )));
      },
    };
    interval = next_interval(interval, &poll);
    let waited = match poll {
      PairPoll::Granted(grant) => Waited::Granted(grant),
      PairPoll::Denied => Waited::Denied,
      PairPoll::Expired => Waited::Expired,
      PairPoll::Pending | PairPoll::SlowDown => continue,
      PairPoll::Other(error) => {
        clear_line(out);
        return Err(SetupError::runtime(format!(
          "toolu.sh refused the pairing poll: {error}"
        )));
      },
    };
    clear_line(out);
    return Ok(waited);
  }
}

/// Sleep until `until` (or `deadline`, if sooner), rewriting the waiting
/// line once a second on a terminal.
async fn tick_until(until: Instant, deadline: Instant, out: Out) {
  let until = until.min(deadline);
  loop {
    let now = Instant::now();
    if now >= until {
      return;
    }
    if !out.plain {
      // `\x1b[5m` blinks the marker; no spinner (the legend).
      print!(
        "\r\x1b[2K│  \x1b[5m●\x1b[25m Waiting for approval…            expires in {}",
        countdown(deadline.saturating_duration_since(now))
      );
      let _ = std::io::stdout().flush();
    }
    tokio::time::sleep((until - now).min(Duration::from_secs(1))).await;
  }
}

/// End the rewritten waiting line (terminal only).
fn clear_line(out: Out) {
  if !out.plain {
    print!("\r\x1b[2K");
    let _ = std::io::stdout().flush();
  }
}

/// "Press enter for a new code" over any key source: enter goes on, ctrl+c
/// cancels (exit 130), esc goes back; other keys are ignored.
pub(crate) fn run_press_enter(
  keys: &mut impl KeySource,
  out: &mut impl std::io::Write,
  text: &str,
) -> Result<(), PromptEnd> {
  let mut write = |text: &str| {
    out
      .write_all(text.as_bytes())
      .and_then(|()| out.flush())
      .map_err(|e| PromptEnd::Io(format!("could not write to the terminal: {e}")))
  };
  write(&format!("\r\x1b[K   {text}"))?;
  loop {
    match keys.next_key()? {
      Key::Enter => return write("\r\n"),
      Key::Esc => return Err(PromptEnd::Back),
      Key::CtrlC => return Err(PromptEnd::Cancelled),
      Key::Up | Key::Down | Key::Space | Key::Backspace | Key::Char(_) => {},
    }
  }
}

#[cfg(test)]
#[path = "tests/toolu_wait.rs"]
mod tests;
