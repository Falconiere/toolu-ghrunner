//! Pre-pull: the decisions around keeping `TOOLU_DAEMON_IMAGE` resident, and
//! the one ordering rule that follows from them.
//!
//! **The listener must not bind until the first pull has settled.** A create
//! against an image that is not resident answers 503, and 503 is not a soft
//! "try again later" on the other side: `classifyVpsStatus` reads it as
//! `unavailable` and `vpsDispositionFor` turns that into fallback *plus a
//! five-minute cooldown on this host*. With one box in the fleet, a daemon
//! that bound early and answered 503 while still pulling would cool the only
//! host down for five minutes on every restart — and a restart is routine
//! (token rotation, a new binary). Binding after the pull costs the same
//! wall-clock time and costs nothing else.
//!
//! The other half is `crates/daemon/README.md`'s standing invariant: the
//! daemon **never pulls inside a request**. The client's timeout is ten
//! seconds and a timeout there is terminal-with-cooldown, so a cold pull in
//! the request path fails the customer's job outright. Every pull happens
//! here — once at startup, then on a timer — and a request that still finds
//! the image absent gets a fast 503 instead.
//!
//! Everything in this module is a pure decision over an observed outcome. The
//! bollard half — the actual pull and the actual presence check — lives in
//! `crate::docker::image`.

/// Whether the pinned image is on the box right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePresence {
  /// `docker inspect` found it: creates can succeed.
  Resident,
  /// Not on the box: every create would 503 until a pull lands.
  Absent,
}

/// Whether one pull call itself succeeded, independent of what is on the box
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullAttempt {
  /// The pull stream ran to completion.
  Succeeded,
  /// The pull failed — registry unreachable, auth rejected, disk full.
  Failed,
}

/// One pre-pull attempt, judged by what the box holds once it settled. A
/// failed pull is only fatal when it leaves nothing behind: an unreachable
/// registry must never take a box with a good image out of service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullOutcome {
  /// The image is resident and the pull that produced it succeeded.
  Pulled,
  /// The pull failed, but an earlier one already left the image resident.
  FailedButResident,
  /// The pull failed and the image is not resident: nothing can run.
  FailedAndAbsent,
}

/// Judge one attempt from the pull's own result and what `docker inspect`
/// then reported.
///
/// A succeeded pull that somehow left no image is treated as
/// [`PullOutcome::FailedAndAbsent`], not as success: the only thing that
/// makes a create work is the image being there, and trusting the pull's
/// exit status over the box's actual contents is how a daemon binds and
/// then 503s every request.
pub fn classify_pull(attempt: PullAttempt, presence: ImagePresence) -> PullOutcome {
  match (attempt, presence) {
    (PullAttempt::Succeeded, ImagePresence::Resident) => PullOutcome::Pulled,
    (PullAttempt::Failed, ImagePresence::Resident) => PullOutcome::FailedButResident,
    (PullAttempt::Succeeded | PullAttempt::Failed, ImagePresence::Absent) => {
      PullOutcome::FailedAndAbsent
    },
  }
}

/// What startup does once a pre-pull attempt has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDecision {
  /// The image is resident: bind the listener and start serving.
  Bind,
  /// Not resident yet: pull again, and keep the listener closed meanwhile.
  Retry,
  /// Not resident and out of attempts: fail startup loudly.
  GiveUp,
}

/// Decide whether serving may begin, given the settled `outcome` and how many
/// pull attempts are still allowed after this one.
///
/// Never [`StartupDecision::Bind`] while the image is absent — that is the
/// whole point of this module. And when the attempts run out, startup fails
/// rather than binding anyway: a process that exits is restarted by its
/// supervisor with a fresh pull, where a listener that only ever answers 503
/// would keep the host in a rolling five-minute cooldown while looking
/// healthy to the tunnel.
pub fn startup_decision(outcome: PullOutcome, attempts_left: u32) -> StartupDecision {
  match outcome {
    PullOutcome::Pulled | PullOutcome::FailedButResident => StartupDecision::Bind,
    PullOutcome::FailedAndAbsent => {
      if attempts_left > 0 {
        StartupDecision::Retry
      } else {
        StartupDecision::GiveUp
      }
    },
  }
}

/// What the periodic refresh does with its outcome. Both variants keep
/// serving: once the listener is bound, nothing a refresh observes may close
/// it. Stopping would cool this host down for five minutes on the client
/// side, while staying up costs at most a 503 per create until the next
/// refresh lands — retryable, and only for as long as the image is genuinely
/// missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshDecision {
  /// The image is resident; carry on.
  KeepServing,
  /// The image is gone and the pull could not replace it — creates will 503
  /// until a later refresh succeeds. Worth a loud log line and nothing else.
  WarnMissingImage,
}

/// Decide what a refresh tick reports, given its settled outcome.
pub fn refresh_decision(outcome: PullOutcome) -> RefreshDecision {
  match outcome {
    PullOutcome::Pulled | PullOutcome::FailedButResident => RefreshDecision::KeepServing,
    PullOutcome::FailedAndAbsent => RefreshDecision::WarnMissingImage,
  }
}

/// The tag Docker's `/images/create` assumes when a reference names none.
const DEFAULT_TAG: &str = "latest";

/// Split an image reference into the `fromImage` and `tag` halves Docker's
/// pull endpoint takes.
///
/// The separator is the last `:` *after* the last `/`, which is what keeps a
/// registry port out of it — `localhost:5000/toolu/runner` is a repository
/// with no tag, not a repository called `localhost` at tag
/// `5000/toolu/runner`. A digest pin (`repo@sha256:…`) splits at the `@` and
/// carries the whole `sha256:…` across as the reference, which is how the
/// Docker CLI pulls one. A reference naming neither gets `latest`, the same
/// default the daemon itself would apply.
pub fn split_image_ref(image: &str) -> (&str, &str) {
  if let Some((repository, digest)) = image.rsplit_once('@') {
    return (repository, digest);
  }
  match image.rsplit_once(':') {
    Some((repository, tag)) if !tag.contains('/') => (repository, tag),
    Some(_) | None => (image, DEFAULT_TAG),
  }
}

#[cfg(test)]
#[path = "tests/prepull.rs"]
mod tests;
