use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

/// Minimum length for a secret (and any derived variant) to be registered.
/// Shorter values are too generic and would cause false-positive masking.
const MIN_SECRET_LEN: usize = 4;

/// Masks secret values in strings.
///
/// Secrets are registered upfront (job Variables + `MaskHints`); each
/// registers its JSON-escaped, base64/hex/percent, and per-line variants
/// too. `patterns` stays sorted longest-first (invariant of
/// `insert_pattern`) — the automaton's source of truth and the fallback
/// loop's ordering. `automaton` (`MatchKind::Standard`, rebuilt once per
/// `add_secret`) is `None` only on a build failure (WARN-logged); `mask`
/// then falls back to the old loop rather than returning input unmasked.
#[derive(Debug, Clone)]
pub struct SecretMasker {
  patterns: Vec<String>,
  automaton: Option<AhoCorasick>,
}

/// Bridge into [`crate::startup::SecretRedactor`] for a `SecretMasker`
/// wrapped in a `Mutex` and shared across the listener, the per-job
/// `ExecutionContext`, and the tracing file sink.
///
/// The Mutex is the gate for `add_secret`; the inner `mask` call is
/// `&self` so it doesn't need exclusive access. Each `redact` call
/// takes the lock briefly to read the current pattern set.
///
/// `SecretMasker` (without the Mutex) already implements `SecretRedactor`
/// directly — see the impl below. Use this wrapper only when the masker
/// is shared as `Arc<Mutex<SecretMasker>>` across multiple threads and
/// must implement `SecretRedactor` for the file sink.
pub struct MaskerRedactor(pub std::sync::Arc<std::sync::Mutex<SecretMasker>>);

impl crate::startup::SecretRedactor for MaskerRedactor {
  fn redact(&self, line: &str) -> String {
    // Use `match` to recover from a poisoned Mutex without using
    // the panic-on-poison convenience. The inner SecretMasker is
    // still valid even if a prior holder panicked.
    let guard = match self.0.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    guard.mask(line)
  }
}

/// Bridge into [`crate::startup::SecretRedactor`].
///
/// `SecretMasker` and the `SecretRedactor` trait both live in `shared`,
/// so this impl is in-crate. It is the one-way wiring the runner uses
/// when calling `crate::startup::init_with_redactor`.
impl crate::startup::SecretRedactor for SecretMasker {
  fn redact(&self, line: &str) -> String {
    self.mask(line)
  }
}

impl SecretMasker {
  /// Create a new empty masker.
  pub fn new() -> Self {
    Self {
      patterns: Vec::new(),
      automaton: None,
    }
  }

  /// Register a secret value to be masked.
  ///
  /// Ignores empty/whitespace-only values and values shorter than 4 characters
  /// (too short, would cause false positives). Rebuilds the matching
  /// automaton once at the end, not per pattern inserted.
  pub fn add_secret(&mut self, value: &str) {
    let trimmed = value.trim();
    if trimmed.len() < MIN_SECRET_LEN {
      return;
    }

    self.add_pattern(trimmed);

    for line in trimmed.split(['\n', '\r']) {
      let line = line.trim();
      if line.len() >= MIN_SECRET_LEN {
        self.add_pattern(line);
      }
    }

    self.rebuild_automaton();
  }

  /// Replace all registered secret patterns with `***` in one pass.
  ///
  /// Falls back to the longest-first `str::replace` loop when the
  /// automaton failed to build — never treats a missing automaton as
  /// "nothing to mask".
  pub fn mask(&self, input: &str) -> String {
    if self.patterns.is_empty() {
      return input.to_owned();
    }
    match &self.automaton {
      Some(automaton) => mask_with_automaton(automaton, input),
      None => self.mask_with_replace_loop(input),
    }
  }

  /// Longest-first `str::replace` loop — the pre-Aho-Corasick behaviour,
  /// kept as the fallback for when `automaton` is `None`.
  fn mask_with_replace_loop(&self, input: &str) -> String {
    let mut result = input.to_owned();
    for pattern in &self.patterns {
      result = result.replace(pattern, "***");
    }
    result
  }

  /// Rebuild `automaton` from `patterns`. A build failure is logged at
  /// WARN and leaves `automaton` as `None`, which routes `mask` to the
  /// `str::replace` fallback rather than skipping masking.
  fn rebuild_automaton(&mut self) {
    self.automaton = match AhoCorasickBuilder::new()
      .match_kind(MatchKind::Standard)
      .build(&self.patterns)
    {
      Ok(automaton) => Some(automaton),
      Err(err) => {
        tracing::warn!(
          error = %err,
          "failed to build secret-masking automaton; falling back to the replace loop"
        );
        None
      },
    };
  }

  fn add_pattern(&mut self, value: &str) {
    self.insert_pattern(value.to_owned());

    let json_escaped = value
      .replace('\\', "\\\\")
      .replace('"', "\\\"")
      .replace('\n', "\\n")
      .replace('\r', "\\r")
      .replace('\t', "\\t");
    if json_escaped != value {
      self.insert_pattern(json_escaped);
    }

    // Encoded forms of the raw secret. Without these, `echo $SECRET |
    // base64`, `Authorization: Basic <base64>`, a hex dump, or a %-encoded
    // token in a URL would land unredacted in the journal / `_diag` log.
    let bytes = value.as_bytes();
    for variant in [
      STANDARD.encode(bytes),
      STANDARD_NO_PAD.encode(bytes),
      hex_encode(bytes, false),
      hex_encode(bytes, true),
      percent_encode(bytes, true),
      percent_encode(bytes, false),
    ] {
      self.insert_variant(variant);
    }
  }

  /// Insert a derived variant, respecting the min-length guard. Duplicates
  /// (and variants that collapse to an already-registered form, e.g. a
  /// percent-encoding with no bytes to escape) are dropped by
  /// [`Self::insert_pattern`].
  fn insert_variant(&mut self, variant: String) {
    if variant.len() >= MIN_SECRET_LEN {
      self.insert_pattern(variant);
    }
  }

  /// Insert one pattern keeping `patterns` sorted longest-first; a duplicate
  /// is ignored. Maintained as an invariant so `mask` never sorts.
  fn insert_pattern(&mut self, pattern: String) {
    if self.patterns.contains(&pattern) {
      return;
    }
    let pos = self.patterns.partition_point(|p| p.len() >= pattern.len());
    self.patterns.insert(pos, pattern);
    debug_assert!(
      self.patterns.is_sorted_by(|a, b| a.len() >= b.len()),
      "patterns must stay sorted longest-first (partition_point relies on it)"
    );
  }
}

impl Default for SecretMasker {
  fn default() -> Self {
    Self::new()
  }
}

/// One overlapping-match pass over `input`, merging overlapping/adjacent
/// spans into a single `***` each. `find_overlapping_iter` matches are not
/// guaranteed sorted by start, so spans are collected and sorted first.
///
/// Never leaks a partial pattern: a merged span is the union of every
/// match that touches it, so `["abcdef12", "xabc"]` over `"xabcdef12y"`
/// yields `"***y"`, not `LeftmostLongest`'s leaking `"***def12y"`.
fn mask_with_automaton(automaton: &AhoCorasick, input: &str) -> String {
  let mut spans: Vec<(usize, usize)> = automaton
    .find_overlapping_iter(input)
    .map(|m| (m.start(), m.end()))
    .collect();
  if spans.is_empty() {
    return input.to_owned();
  }
  spans.sort_unstable();

  let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
  for (start, end) in spans {
    if let Some((_, last_end)) = merged.last_mut()
      && start <= *last_end
    {
      if end > *last_end {
        *last_end = end;
      }
      continue;
    }
    merged.push((start, end));
  }

  let mut result = String::with_capacity(input.len());
  let mut cursor = 0;
  for (start, end) in merged {
    result.push_str(input.get(cursor..start).unwrap_or_default());
    result.push_str("***");
    cursor = end;
  }
  result.push_str(input.get(cursor..).unwrap_or_default());
  result
}

/// One hex digit for `nibble` (0..=15). `upper` picks `A-F` vs `a-f`.
fn hex_nibble(nibble: u8, upper: bool) -> char {
  let c = char::from_digit(u32::from(nibble), 16).unwrap_or('0');
  if upper { c.to_ascii_uppercase() } else { c }
}

/// Hex-encode `bytes`; `upper` selects uppercase digits.
fn hex_encode(bytes: &[u8], upper: bool) -> String {
  let mut out = String::with_capacity(bytes.len() * 2);
  for &b in bytes {
    out.push(hex_nibble(b >> 4, upper));
    out.push(hex_nibble(b & 0x0f, upper));
  }
  out
}

/// RFC 3986 percent-encode `bytes`: every byte outside the unreserved set
/// (`A-Za-z0-9-._~`) becomes `%XX`. `upper` selects the case of the hex
/// digits in each escape.
fn percent_encode(bytes: &[u8], upper: bool) -> String {
  let mut out = String::with_capacity(bytes.len());
  for &b in bytes {
    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
      out.push(char::from(b));
    } else {
      out.push('%');
      out.push(hex_nibble(b >> 4, upper));
      out.push(hex_nibble(b & 0x0f, upper));
    }
  }
  out
}

// Only the automaton-build-failure fallback lives here: it must construct
// `SecretMasker` with `automaton: None` directly, which needs the private
// field — unreachable from `tests/secret_masker_test.rs`, where the rest
// of this file's test coverage (overlap no-leak, multi-line, encoded
// variants) lives against the public API.
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn automaton_none_falls_back_to_replace_loop_and_still_masks() {
    let mut masker = SecretMasker::new();
    masker.add_secret("fallback-secret-value-9");
    masker.automaton = None; // simulate a build failure
    let out = masker.mask("token=fallback-secret-value-9 tail");
    assert_eq!(out, "token=*** tail", "fallback loop must still mask: {out}");
  }
}
