# secret-content.sh — a file's CONTENT must never hold a live-looking secret.
#
# secrets.sh bans a handful of secret FILES (.dev.vars, .env) from ever being
# tracked; this is the complement. A private key or an API token pasted inline
# in ordinary source, a script, or a config is just as real a leak, and ships
# in files this kit otherwise welcomes tracked.
#
# One `grep -E -I -n` sweep per file, every pattern combined with `|` rather
# than one grep per pattern per file — the same "one process, not N" reasoning
# file-size.sh and patterns.sh already document.  -I skips binaries, so the
# scan never chokes on an image or a lockfile's binary-looking blob.
#
# scripts/guardrails/ is ALWAYS exempt, unconditionally: the kit's own test
# fixtures plant fake secrets on purpose (this check's own fixture among them),
# and every scaffolded project carries that whole directory verbatim. Without
# the exemption every project this kit produces would fail its own gate the
# moment it copied the kit in.

GR_SC_PATTERN='-----BEGIN [A-Z ]*PRIVATE KEY-----|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{22}|xox[baprs]-[0-9A-Za-z-]{10}|sk_live_[0-9A-Za-z]{20}|AIza[0-9A-Za-z_-]{35}'

# gr_sc_kind <matched-text> — which pattern matched, for the violation message.
gr_sc_kind() {
  case "$1" in
    *'PRIVATE KEY-----'*) printf 'private key' ;;
    *AKIA*) printf 'AWS access key id' ;;
    *gh[pousr]_*) printf 'GitHub token' ;;
    *github_pat_*) printf 'GitHub token' ;;
    *xox[baprs]-*) printf 'Slack token' ;;
    *sk_live_*) printf 'Stripe live key' ;;
    *AIza*) printf 'Google API key' ;;
    *) printf 'secret' ;;
  esac
}

# gr_sc_exempt <path> — scripts/guardrails/ is exempt unconditionally;
# secrets.scanExempt adds more, project by project.
gr_sc_exempt() {
  case "$1" in
    scripts/guardrails/*) return 0 ;;
  esac
  local glob
  for glob in $GR_SECRET_SCAN_EXEMPT; do
    # shellcheck disable=SC2254
    case "$1" in $glob) return 0 ;; esac
  done
  return 1
}

# gr_sc_scan <path> — one grep, first match wins: a file with two leaked
# secrets still only needs the one fix instruction to start acting on.
gr_sc_scan() {
  local path match kind errfile errtext status
  path=$1
  gr_sc_exempt "$path" && return 0
  [ -f "$path" ] || return 0
  # -e, not a bare pattern argument: the private-key pattern itself starts with
  # a literal "-", which grep would otherwise parse as an option cluster and
  # fail with "unrecognized option" instead of scanning anything.
  #
  # The `--` sentinel guards the OTHER side of that same class of bug: $path is
  # untrusted too (it is a tracked filename, chosen by whoever committed it),
  # and a dash-prefixed name (e.g. a file literally called "-c") is otherwise
  # parsed as an option cluster of its own, leaving grep with no file operand
  # at all — at which point it silently reads STDIN instead of the file. On a
  # single --file invocation that just means the wrong thing gets scanned; in
  # the old per-file repo-mode loop it was worse — see gr_sc_repo_list below.
  #
  # stderr is captured, never discarded: grep exiting 1 with a quiet stderr is
  # the one legitimate "clean file" outcome. Anything else — a diagnostic on
  # stderr, or an exit status above 1 (unreadable file, EISDIR) — is a scan
  # that did NOT happen, and a scan that did not happen must fail the gate
  # closed, not report the file as clean. Same doctrine as the batch pass and
  # patterns.sh.
  errfile=$(mktemp)
  match=$(grep -E -I -n -m1 -e "$GR_SC_PATTERN" -- "$path" 2>"$errfile")
  status=$?
  errtext=$(cat "$errfile")
  rm -f "$errfile"
  if [ -n "$errtext" ] || [ "$status" -gt 1 ]; then
    gr_fatal "secret-content scan failed on $path: ${errtext:-grep exited $status}"
  fi
  [ -n "$match" ] || return 0
  kind=$(gr_sc_kind "${match#*:}")
  gr_violation secret-content "$path" \
    "committed secret value ($kind)" \
    'rotate the credential, remove it from this file, and load it from .dev.vars/env instead'
}

# gr_sc_repo_list <dest-file> — every tracked path this scan should look at,
# NUL-delimited, written to $1.
#
# `git ls-files -z` + `read -r -d ''`, not plain `git ls-files` + `read -r`:
# under the default core.quotepath=true, plain `git ls-files` C-quotes any
# filename holding a non-ASCII byte (café.ts prints as "caf\303\251.ts",
# quotes included), and that quoted string then fails a plain `[ -f "$path" ]`
# — a silent per-file bypass for exactly the filenames quoting exists to
# protect. -z always emits raw bytes, never quoted, so this is the one form
# that cannot be fooled by the filename it names.
#
# This is also what fixes the STDIN-drain risk from a dash-prefixed filename
# (see gr_sc_scan's comment) for repo mode specifically: the exemption/`-f`
# checks below run entirely in bash — no subprocess, nothing that could ever
# fall back to reading this loop's own input — so there is no shared file
# descriptor left for a misbehaving grep to drain. grep itself only runs once,
# afterward, in gr_sc_scan_batch, decoupled from this loop's stdin entirely.
gr_sc_repo_list() {
  local dest f
  dest=$1
  while IFS= read -r -d '' f; do
    gr_sc_exempt "$f" && continue
    [ -f "$f" ] || continue
    printf '%s\0' "$f"
  done < <(git ls-files -z) > "$dest"
}

# gr_sc_scan_batch <nul-delimited path list> — same contract as gr_sc_scan
# (first match wins, per file) but ONE grep process for every surviving file
# instead of one process per file. Other checks batch on purpose — file-size.sh
# and filename-case.sh document the same "one process, not N" reasoning; a
# grep fork per tracked file was the one thing left paying per-file process
# overhead in this check.
#
#   xargs -0 grep -E -I -l -Z -e "$PATTERN" --
#
# -l -Z, not -n -H: the batch pass only asks WHICH files leak, as a
# NUL-delimited filename list. Parsing "path:line:content" text back apart is
# unfixably ambiguous — a colon is legal in both the filename and the matched
# content, so no split direction is safe — while -Z terminates each filename
# with a NUL that no filename can contain. The one thing the text output
# carried that violations actually use, the matched text behind the kind
# label, comes from re-running gr_sc_scan on just the offending files below:
# leaks are the rare case (normally zero files), so the second pass is one
# grep per LEAKING file, not per tracked file, and the "one process, not N"
# batching win is untouched. (-l also already stops each file at its first
# match, so -m1 has nothing left to cap.)
#
# Exit-code contract (read this twice before touching it):
# grep on N>=1 files returns 0 (matched somewhere), 1 (matched nowhere), or
# >=2 (a real failure: unreadable file, EISDIR, etc — -f above should have
# already filtered non-files out, but a permission-denied file can still
# reach here). xargs then remaps ITS OWN exit status from whatever its child
# returned: 0 stays 0, any of 1..125 collapses to 123, 126/127 pass through
# unchanged, a killing signal becomes 128+signal. Bare grep under xargs would
# therefore be ambiguous — 1 (clean no-match) and >=2 (real error) both land
# in that 1..125 band, and with a survivor list large enough to split across
# several grep invocations, one batch's error and another's clean no-match
# collapse into the same 123.
#
# So grep runs under a one-line sh wrapper that absorbs exit 1 — the ONE
# status that legitimately means "scanned everything, found nothing" — and
# lets every other failure through. After that remap the xargs status is
# unambiguous and fail-closed on its own: 0 = every file scanned (matches or
# not), anything else = at least one batch did NOT complete its scan, and a
# scan that did not happen must never pass the gate. grep's stderr is
# captured as a second, independent tripwire and any output there also fails
# the run closed — belt and braces, since this is the check other checks
# trust to be paranoid.
gr_sc_scan_batch() {
  local list errfile hitfile status path errtext
  list=$1
  errfile=$(mktemp)
  # stdout goes to a file, not $(...): the -Z output is NUL-delimited and
  # command substitution silently drops NUL bytes.
  hitfile=$(mktemp)
  xargs -0 sh -c 'pat=$1; shift; grep -E -I -l -Z -e "$pat" -- "$@" || [ $? -eq 1 ]' sh "$GR_SC_PATTERN" \
    < "$list" > "$hitfile" 2>"$errfile"
  status=$?
  errtext=$(cat "$errfile")
  rm -f "$errfile"
  [ -z "$errtext" ] || { rm -f "$hitfile"; gr_fatal "secret-content repo scan failed: $errtext"; }
  case "$status" in
    0) ;; # every batch completed its scan — with or without matches
    *) rm -f "$hitfile"; gr_fatal "secret-content repo scan: batch exited $status — a batch did not finish scanning" ;;
  esac
  while IFS= read -r -d '' path; do
    gr_sc_scan "$path"
  done < "$hitfile"
  rm -f "$hitfile"
}

gr_check_secret_content() {
  mode=$1
  path=${2-}

  if [ "$mode" = 'file' ]; then
    gr_sc_scan "$path"
    return 0
  fi

  [ "$mode" = 'repo' ] || return 0
  git rev-parse --git-dir >/dev/null 2>&1 || return 0

  list=$(mktemp)
  gr_sc_repo_list "$list"
  if [ -s "$list" ]; then
    gr_sc_scan_batch "$list"
  fi
  rm -f "$list"
}
