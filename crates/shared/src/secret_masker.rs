/// Masks secret values in strings.
///
/// Secrets are registered upfront (from job Variables with `IsSecret=true`
/// and `MaskHints`). Each secret is also split on newlines and each line
/// registered separately. JSON-escaped variants are auto-registered.
///
/// `patterns` is held sorted longest-first as an invariant (maintained by
/// `add_pattern`), so the per-line `mask` hot path does no per-call sort or
/// allocation — it just runs the replace passes in order.
#[derive(Debug, Clone)]
pub struct SecretMasker {
  patterns: Vec<String>,
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
    }
  }

  /// Register a secret value to be masked.
  ///
  /// Ignores empty/whitespace-only values and values shorter than 4 characters
  /// (too short, would cause false positives).
  pub fn add_secret(&mut self, value: &str) {
    let trimmed = value.trim();
    if trimmed.len() < 4 {
      return;
    }

    self.add_pattern(trimmed);

    for line in trimmed.split(['\n', '\r']) {
      let line = line.trim();
      if line.len() >= 4 {
        self.add_pattern(line);
      }
    }
  }

  /// Replace all registered secret patterns with `***`.
  ///
  /// `patterns` is kept sorted longest-first (see `insert_pattern`), so a
  /// longer secret is replaced before any shorter substring of it — no
  /// per-call sort or allocation on this hot path.
  pub fn mask(&self, input: &str) -> String {
    if self.patterns.is_empty() {
      return input.to_owned();
    }
    let mut result = input.to_owned();
    for pattern in &self.patterns {
      result = result.replace(pattern, "***");
    }
    result
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
