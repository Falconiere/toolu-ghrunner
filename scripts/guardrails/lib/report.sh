# report.sh — uniform violation reporting for every guardrails check.
#
# One format, everywhere:
#   guardrails[<check-id>] <path>: <problem> — <remedy>
#
# The remedy clause is mandatory, not decorative. These messages are read by an
# agent mid-turn through a PostToolUse hook; one told only what is wrong retries
# blindly, which is how a guard rail turns into a loop. Say what to do instead.

# Set to 1 by gr_violation. run.sh reads it to pick an exit code.
GR_FAIL=0

# Prefix prepended to every reported path. Empty in the single-repo case, where
# output must stay byte-identical. In a workspace, run.sh re-execs itself once
# per package with the working directory INSIDE that package, so every check
# reports package-relative paths ("src/foo.ts") with nothing saying which
# package. The parent exports this so the violation reads
# "packages/database/src/foo.ts" — the path a person can actually open.
GR_PATH_PREFIX=${GR_PATH_PREFIX-}

# gr_violation <check-id> <path> <problem> <remedy>
gr_violation() {
  printf 'guardrails[%s] %s%s: %s — %s\n' "$1" "$GR_PATH_PREFIX" "$2" "$3" "$4" >&2
  GR_FAIL=1
}

# gr_fatal <message> — the guardrail itself is broken or misconfigured. Exits 3,
# never 1: a broken check must never be mistakable for a clean run.
gr_fatal() {
  printf 'guardrails: %s\n' "$1" >&2
  exit 3
}

# gr_warn <message> — advisory only, never affects the exit code.
gr_warn() {
  printf 'guardrails: warning: %s\n' "$1" >&2
}
