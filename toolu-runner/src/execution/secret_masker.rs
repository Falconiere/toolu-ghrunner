/// Masks secret values in strings.
///
/// Secrets are registered upfront (from job Variables with `IsSecret=true`
/// and `MaskHints`). Each secret is also split on newlines and each line
/// registered separately. JSON-escaped variants are auto-registered.
#[derive(Debug, Clone)]
pub struct SecretMasker {
  patterns: Vec<String>,
}

/// Bridge into `shared::startup::SecretRedactor` for a `SecretMasker`
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

impl shared::startup::SecretRedactor for MaskerRedactor {
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

/// Bridge into `shared::startup::SecretRedactor`.
///
/// `SecretMasker` lives in `toolu-runner` but the trait lives in `shared`
/// (so `shared` never has to depend on the runner). This impl is the
/// one-way wiring the runner uses when calling
/// `shared::startup::init_with_redactor`.
impl shared::startup::SecretRedactor for SecretMasker {
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
  /// Patterns are replaced longest-first to avoid partial matches.
  pub fn mask(&self, input: &str) -> String {
    if self.patterns.is_empty() {
      return input.to_owned();
    }
    let mut result = input.to_owned();
    let mut sorted: Vec<&str> = self.patterns.iter().map(String::as_str).collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for pattern in sorted {
      result = result.replace(pattern, "***");
    }
    result
  }

  fn add_pattern(&mut self, value: &str) {
    if self.patterns.iter().any(|p| p == value) {
      return;
    }
    self.patterns.push(value.to_owned());

    let json_escaped = value
      .replace('\\', "\\\\")
      .replace('"', "\\\"")
      .replace('\n', "\\n")
      .replace('\r', "\\r")
      .replace('\t', "\\t");
    if json_escaped != value && !self.patterns.iter().any(|p| p == &json_escaped) {
      self.patterns.push(json_escaped);
    }
  }
}

impl Default for SecretMasker {
  fn default() -> Self {
    Self::new()
  }
}
