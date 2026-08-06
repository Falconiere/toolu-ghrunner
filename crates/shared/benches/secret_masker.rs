#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Criterion bench for `SecretMasker::mask` at 100 registered patterns.
//!
//! Registers the real corpus's 3 secrets (`~15` patterns after variants)
//! plus 15 synthesized, realistically-named secrets to reach exactly 100
//! registered patterns — the routine per-job scale this rewrite targets
//! (T3 in `docs/toolu/specs/2026-08-05-job-wallclock-tier1-design.md`).
//! Runs `mask()` over every line of the real fixture, corpus copied to
//! `benches/fixtures/` per-crate rather than `include_str!`-ed across
//! crate layers (`shared` is the workspace's lowest crate).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use shared::SecretMasker;

/// The corpus, copied verbatim from
/// `crates/toolu-runner/tests/fixtures/secret-masking-input.txt`.
const CORPUS: &str = include_str!("fixtures/secret-masking-input.txt");

/// Secret shapes actually present in `CORPUS` — see
/// `crates/toolu-runner/tests/secret_masker_real_test.rs`.
const FIXTURE_SECRETS: &[&str] = &[
  "hunter2-token-value-abc123",
  "ghp_FAKEghp_FAKEghp_FAKEghp_FAKE",
  "abc-token-value-abc-token-value-abc",
];

/// Synthesized secret name prefixes; each becomes one registered secret
/// (`"<prefix>-VALUE-NNN!"`). Chosen (length, punctuation) so that, together
/// with `FIXTURE_SECRETS`, the masker registers exactly 100 patterns after
/// JSON-escaped/base64/hex/percent variant expansion.
const SYNTHETIC_SECRET_PREFIXES: &[&str] = &[
  "aws-secret-access-key",
  "database-password",
  "oauth-client-secret",
  "stripe-api-key",
  "npm-publish-token",
  "docker-registry-password",
  "slack-webhook-token",
  "sendgrid-api-key",
  "jwt-signing-secret",
  "ssh-deploy-key-passphrase",
  "terraform-cloud-token",
  "datadog-api-key",
  "pypi-upload-token",
  "cloudflare-api-token",
  "vault-unseal-key",
];

/// Build a masker with 100 registered patterns: the real corpus's 3
/// secrets plus 15 synthesized ones.
fn build_masker_at_100_patterns() -> SecretMasker {
  let mut masker = SecretMasker::new();
  for secret in FIXTURE_SECRETS {
    masker.add_secret(secret);
  }
  for (i, prefix) in SYNTHETIC_SECRET_PREFIXES.iter().enumerate() {
    masker.add_secret(&format!("{prefix}-VALUE-{:03}!", i + 1));
  }
  masker
}

fn bench_mask(c: &mut Criterion) {
  let masker = build_masker_at_100_patterns();
  let lines: Vec<&str> = CORPUS.lines().collect();

  c.bench_function("secret_masker_mask_100_patterns", |b| {
    b.iter(|| {
      for line in &lines {
        black_box(masker.mask(black_box(line)));
      }
    });
  });
}

criterion_group!(benches, bench_mask);
criterion_main!(benches);
