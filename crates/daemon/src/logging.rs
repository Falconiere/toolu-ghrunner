//! The subscriber filter this daemon installs, and the one thing it will not
//! let an operator turn up.
//!
//! bollard logs every request it sends at `TRACE`, body included
//! (`trace!("request: {bytes:?}")`). The body of `POST /containers/create` is
//! the container's `Env` array, and that array carries `TOOLU_JITCONFIG` — a
//! single-use GitHub credential the rest of this crate goes out of its way to
//! keep out of argv, out of labels and out of `docker inspect`
//! (`crate::docker::spec`). Nothing today reaches that log line only because
//! the process hardcoded its filter and ignored `RUST_LOG` entirely; the
//! natural next change — "make logging configurable" — would have started
//! printing job credentials to the journal.
//!
//! So the filter is configurable *and* pinned: `RUST_LOG` decides everything
//! except bollard's own ceiling, which is appended last and therefore wins
//! (`EnvFilter` replaces an earlier directive with a later one of the same
//! specificity). `RUST_LOG=trace` is a legitimate thing for an operator to
//! reach for on a box that will not start a container; it must not be the
//! thing that leaks the credentials of every job on that box.

use tracing_subscriber::EnvFilter;

/// What the daemon logs at when `RUST_LOG` is unset — the level every
/// `tracing::info!` in this crate is written for.
const DEFAULT_DIRECTIVES: &str = "info";

/// bollard's ceiling, appended after the operator's directives so it wins.
/// `info` rather than `warn`: bollard says nothing at `info`, and the point is
/// the level below it, not silence.
const BOLLARD_CEILING: &str = "bollard=info";

/// The filter to install, given `requested` — the raw `RUST_LOG` value
/// (`crate::config::log_directives`), or `None` when it is unset.
///
/// A `RUST_LOG` that does not parse falls back to [`DEFAULT_DIRECTIVES`]
/// whole. The lenient parse the final `EnvFilter::new` performs would instead
/// drop just the bad directive, and since a set that parsed *something* gets
/// no default directive added, one typo would leave the daemon logging
/// nothing about itself while still running — silence that reads exactly like
/// a daemon that has stopped working. Never fatal, either way: a mistyped
/// environment variable is not a reason to refuse to boot a box.
pub fn log_filter(requested: Option<&str>) -> EnvFilter {
  let requested = requested
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .unwrap_or(DEFAULT_DIRECTIVES);
  let directives = match EnvFilter::builder().parse(requested) {
    Ok(_parsed) => requested,
    Err(err) => {
      eprintln!("ignoring an unusable RUST_LOG ({err}); logging at {DEFAULT_DIRECTIVES}");
      DEFAULT_DIRECTIVES
    },
  };
  EnvFilter::new(format!("{directives},{BOLLARD_CEILING}"))
}

#[cfg(test)]
#[path = "tests/logging.rs"]
mod tests;
