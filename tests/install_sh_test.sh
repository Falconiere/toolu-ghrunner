#!/usr/bin/env bash
# Tests for install.sh.
#
# Run as: bash tests/install_sh_test.sh            # every group
#         bash tests/install_sh_test.sh S1 S2      # only those groups
# Groups: legacy (the --check / flag smoke tests) and the #200 scenarios
# S1 S1b S2 S3 S4 S5 S5b S6 S6b S7 S8 S8b S10.
#
# The scenarios run the real script against a real python3 HTTP server on
# 127.0.0.1 that serves release fixtures built by scripts/package-release.sh,
# with SHA256SUMS made by the release workflow's own command, into temporary
# HOME directories, then check what real bash / zsh / fish login shells see.
# They use the real built toolu-runner (target/debug, built when missing; or
# TOOLU_RUNNER_TEST_BIN). The setup handoff uses a fixture binary that
# records its argv; the real binary's own `setup` (#204) is covered by the
# Rust scenario tests in crates/toolu-runner/tests/setup_cmd_test.rs.
#
# Requires python3, setsid, script (util-linux), timeout, zsh and fish. A
# missing one FAILS the run: it must never look like a pass.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$ROOT/install.sh"

if [[ ! -f "$SCRIPT" ]]; then
  echo "FAIL: $SCRIPT not found" >&2
  exit 1
fi

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

GROUPS_WANTED=("$@")
want() {
  [[ ${#GROUPS_WANTED[@]} -eq 0 ]] && return 0
  local g
  for g in "${GROUPS_WANTED[@]}"; do
    [[ "$g" == "$1" ]] && return 0
  done
  return 1
}

tmp="$(mktemp -d -t install-sh-test.XXXXXX)"
SERVER_PID=""
# The host's real install target must never be touched by a test.
host_bin_state() { ls -li /usr/local/bin/toolu-runner 2>/dev/null || echo absent; }
HOST_BIN_BEFORE="$(host_bin_state)"
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  chmod -R u+w "$tmp" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

# =========================================================================
# legacy: --help / --check / flag parsing / install dir resolution
# =========================================================================
if want legacy; then
  echo "legacy 1: --help"
  help_out="$("$SCRIPT" --help)"
  echo "$help_out" | grep -q "install.sh" || fail "--help output missing 'install.sh'"
  echo "$help_out" | grep -q -- "--no-setup" || fail "--help should document --no-setup"
  echo "$help_out" | grep -q -- "--uninstall" || fail "--help should document --uninstall"
  echo "$help_out" | grep -q -- "bash -s -- --toolu" || fail "--help should show the --toolu one-liner"
  if echo "$help_out" | grep -q 'registration_token\|--token <reg_token>'; then
    fail "--help still asks for a registration token"
  fi
  if grep -q 'registration_token' "$SCRIPT"; then
    fail "install.sh still mentions a registration token"
  fi

  "$SCRIPT" -h >/dev/null || fail "-h should exit 0"

  echo "legacy 3: --check"
  output="$("$SCRIPT" --check)"
  echo "$output" | grep -q 'arch:'         || fail "--check missing 'arch:'"
  echo "$output" | grep -q 'os:'           || fail "--check missing 'os:'"
  echo "$output" | grep -q 'version:'      || fail "--check missing 'version:'"
  echo "$output" | grep -q 'install dir:'  || fail "--check missing 'install dir:'"
  echo "$output" | grep -q 'would download:' || fail "--check missing 'would download:'"
  echo "$output" | grep -q 'would verify:  .*/releases/latest/download/SHA256SUMS' \
    || fail "--check should name the SHA256SUMS it verifies against: $output"
  echo "$output" | grep -q 'setup args:    (none)' || fail "--check should list no setup args: $output"

  echo "$output" | grep -q 'github.com/.*/releases/latest/download/' \
    || fail "--check should mention releases/latest/download"

  out_v="$("$SCRIPT" --check --version v0.1.0)"
  echo "$out_v" | grep -q '/releases/download/v0.1.0/' \
    || fail "--check --version should pin to /releases/download/<v>/"
  echo "$out_v" | grep -q 'version:       v0.1.0' \
    || fail "--check --version should show pinned version"

  out_d="$("$SCRIPT" --check --install-dir /opt/bin)"
  echo "$out_d" | grep -q 'install dir:   /opt/bin' \
    || fail "--check --install-dir should echo the override"

  out_s="$("$SCRIPT" --check --service --install-dir /usr/local/bin)"
  echo "$out_s" | grep -q 'service:       yes' \
    || fail "--check --service --install-dir /usr/local/bin should show service: yes"

  out_s2="$("$SCRIPT" --check --service --install-dir /opt/bin)"
  echo "$out_s2" | grep -q 'service:       skipped (static unit expects /usr/local/bin/toolu-runner' \
    || fail "--check --service --install-dir /opt/bin should show service: skipped"
  echo "$out_s2" | grep -q 'toolu-runner install-service' \
    || fail "skipped service line should point at toolu-runner install-service"

  echo "legacy 8: bash -n"
  bash -n "$SCRIPT" || fail "bash -n $SCRIPT failed"

  # Unknown options are setup options now: --check lists them in order.
  echo "legacy 9: setup args pass through"
  out_sa="$("$SCRIPT" --check --source github --org acme --no-modify-path -- --yes)"
  echo "$out_sa" | grep -q 'setup args:    --source github --org acme --yes$' \
    || fail "--check should list setup args in order, installer flags removed: $out_sa"
  echo "$out_sa" | grep -q 'modify path:   no (--no-modify-path)' \
    || fail "--check should report --no-modify-path: $out_sa"
  out_ns="$("$SCRIPT" --check --no-setup)"
  echo "$out_ns" | grep -q 'setup:         skipped (--no-setup)' \
    || fail "--check --no-setup should report the skipped handoff: $out_ns"

  # The console one-liner is `bash -s -- --toolu <token>`. setup's flag is
  # `--enroll-token`; the token must not be printed.
  echo "legacy 9b: --toolu becomes --enroll-token"
  tok="toolu-test-enroll-6c2e"
  out_tu="$("$SCRIPT" --check --toolu "$tok")"
  echo "$out_tu" | grep -q 'setup args:    --enroll-token ••••$' \
    || fail "--toolu should become a redacted --enroll-token: $out_tu"
  if echo "$out_tu" | grep -q -- "$tok"; then fail "--check echoed the enrollment token: $out_tu"; fi
  out_tu_eq="$("$SCRIPT" --check --labels gpu --toolu="$tok")"
  echo "$out_tu_eq" | grep -q 'setup args:    --labels gpu --enroll-token ••••$' \
    || fail "--toolu= should rewrite and keep earlier setup flags: $out_tu_eq"
  if echo "$out_tu_eq" | grep -q -- "$tok"; then fail "--toolu= echoed the token: $out_tu_eq"; fi
  out_pipe="$(bash -s -- --check --toolu "$tok" <"$SCRIPT")"
  echo "$out_pipe" | grep -q 'setup args:    --enroll-token ••••$' \
    || fail "bash -s -- --toolu should rewrite: $out_pipe"
  rc_tu=0
  err_tu="$("$SCRIPT" --check --toolu 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "--toolu with no value should exit 2 (rc=$rc_tu): $err_tu"
  echo "$err_tu" | grep -q -- '--toolu requires an enrollment token' \
    || fail "missing --toolu should name the flag: $err_tu"
  rc_tu=0
  err_tu="$("$SCRIPT" --toolu --check 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "--toolu must not swallow the next flag (rc=$rc_tu): $err_tu"
  rc_tu=0
  err_tu="$("$SCRIPT" --check --toolu= 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "an empty --toolu= should exit 2 (rc=$rc_tu): $err_tu"
  rc_tu=0
  err_tu="$("$SCRIPT" --check --toolu "" 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "an empty --toolu value should exit 2 (rc=$rc_tu): $err_tu"
  rc_tu=0
  err_tu="$("$SCRIPT" --check --toolu "$tok" --toolu "$tok" 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "--toolu twice should exit 2 (rc=$rc_tu): $err_tu"
  if echo "$err_tu" | grep -q -- "$tok"; then fail "a --toolu error echoed the token: $err_tu"; fi
  rc_tu=0
  err_tu="$("$SCRIPT" --check --toolu "$tok" --enroll-token "$tok" 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "--toolu with --enroll-token should exit 2 (rc=$rc_tu): $err_tu"
  echo "$err_tu" | grep -q -- '--toolu and --enroll-token cannot both be given' \
    || fail "conflict should name both flags: $err_tu"
  if echo "$err_tu" | grep -q -- "$tok"; then fail "a conflict error echoed the token: $err_tu"; fi
  out_sep="$("$SCRIPT" --check -- --toolu "$tok")"
  echo "$out_sep" | grep -q 'setup args:    --enroll-token ••••$' \
    || fail "-- --toolu should still rewrite: $out_sep"
  rc_tu=0
  err_tu="$("$SCRIPT" --no-setup --toolu "$tok" 2>&1)" || rc_tu=$?
  [[ "$rc_tu" -eq 2 ]] || fail "--no-setup --toolu should exit 2 (rc=$rc_tu): $err_tu"
  if echo "$err_tu" | grep -q -- "$tok"; then fail "--no-setup --toolu echoed the token: $err_tu"; fi

  # --opt=value forms are the installer's, not setup's.
  out_eq="$("$SCRIPT" --check --version=v1.2.3 --install-dir=/opt/bin --home=/srv/toolu)"
  echo "$out_eq" | grep -q 'version:       v1.2.3$' || fail "--version=v1.2.3 should pin the version: $out_eq"
  echo "$out_eq" | grep -q 'install dir:   /opt/bin$' || fail "--install-dir=/opt/bin should be used: $out_eq"
  echo "$out_eq" | grep -q 'setup args:    (none)' || fail "--opt=value forms must not reach setup: $out_eq"
  if "$SCRIPT" --check --home=relative >/dev/null 2>&1; then fail "--home=relative should be rejected"; fi
  for empty in --version= --install-dir= --home=; do
    if "$SCRIPT" --check "$empty" >/dev/null 2>&1; then fail "an empty $empty should be rejected"; fi
  done
  if "$SCRIPT" --check --install-dir "" >/dev/null 2>&1; then fail "an empty --install-dir should be rejected"; fi
  # A relative --install-dir is resolved against the working directory.
  out_rel="$(cd "$tmp" && "$SCRIPT" --check --install-dir rel/bin)"
  echo "$out_rel" | grep -q "install dir:   $tmp/rel/bin\$" || fail "relative --install-dir should be made absolute: $out_rel"

  if "$SCRIPT" --check --version not-a-version >/dev/null 2>&1; then
    fail "--version not-a-version should be rejected"
  fi
  if "$SCRIPT" --check --version 0.1.0 >/dev/null 2>&1; then
    fail "--version 0.1.0 (missing 'v' prefix) should be rejected"
  fi
  if "$SCRIPT" --version >/dev/null 2>&1; then
    fail "--version with no value should be rejected"
  fi
  if "$SCRIPT" --install-dir >/dev/null 2>&1; then
    fail "--install-dir with no value should be rejected"
  fi
  if "$SCRIPT" --home >/dev/null 2>&1; then
    fail "--home with no value should be rejected"
  fi
  "$SCRIPT" --check --service >/dev/null || fail "--service as flag should be accepted"

  # --- install dir resolution (fallback when the default is not writable) ---
  # TOOLU_RUNNER_INSTALL_DIR stands in for /usr/local/bin so the test does not
  # depend on the host's permissions. Root ignores mode bits, so the
  # "unwritable" cases are skipped when running as root.
  echo "legacy 14: install dir resolution"
  fake_home="$tmp/legacy-home"
  rw_dir="$tmp/rw"
  ro_dir="$tmp/ro"
  mkdir -p "$fake_home" "$rw_dir" "$ro_dir"
  chmod 0500 "$ro_dir"

  out_rw="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$rw_dir" "$SCRIPT" --check)"
  echo "$out_rw" | grep -q "install dir:   $rw_dir\$" \
    || fail "writable preferred dir should be used: $out_rw"
  if echo "$out_rw" | grep -q 'note:'; then
    fail "writable preferred dir should print no fallback note"
  fi

  out_new="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$rw_dir/sub/bin" "$SCRIPT" --check)"
  echo "$out_new" | grep -q "install dir:   $rw_dir/sub/bin\$" \
    || fail "creatable preferred dir should be used: $out_new"

  if [[ "$(id -u)" -ne 0 ]]; then
    out_ro="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$ro_dir" "$SCRIPT" --check)"
    echo "$out_ro" | grep -q "install dir:   $fake_home/.local/bin\$" \
      || fail "unwritable preferred dir should fall back to \$HOME/.local/bin: $out_ro"
    echo "$out_ro" | grep -q "note:          $ro_dir is not writable; using $fake_home/.local/bin" \
      || fail "fallback should print a note naming both dirs: $out_ro"
    echo "$out_ro" | grep -q "would install to: $fake_home/.local/bin/toolu-runner" \
      || fail "fallback should install under \$HOME/.local/bin: $out_ro"

    out_ro2="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$ro_dir/bin" "$SCRIPT" --check)"
    echo "$out_ro2" | grep -q "install dir:   $fake_home/.local/bin\$" \
      || fail "uncreatable preferred dir should fall back: $out_ro2"

    out_ro_s="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$ro_dir" "$SCRIPT" --check --service)"
    echo "$out_ro_s" | grep -q 'service:       skipped' \
      || fail "--service with a fallback dir should be skipped: $out_ro_s"

    out_explicit="$(HOME="$fake_home" TOOLU_RUNNER_INSTALL_DIR="$rw_dir" "$SCRIPT" --check --install-dir "$ro_dir")"
    echo "$out_explicit" | grep -q "install dir:   $ro_dir\$" \
      || fail "explicit --install-dir should be used as given: $out_explicit"
    if echo "$out_explicit" | grep -q 'note:'; then
      fail "explicit --install-dir should print no fallback note"
    fi

    out_nohome="$(env -u HOME TOOLU_RUNNER_INSTALL_DIR="$ro_dir" "$SCRIPT" --check)"
    echo "$out_nohome" | grep -q "install dir:   $ro_dir\$" \
      || fail "with HOME unset the default dir should be kept: $out_nohome"

    out_slash="$("$SCRIPT" --check --service --install-dir /usr/local/bin/)"
    echo "$out_slash" | grep -q 'install dir:   /usr/local/bin$' \
      || fail "trailing slash should be stripped from --install-dir: $out_slash"
    echo "$out_slash" | grep -q 'service:       yes' \
      || fail "--install-dir /usr/local/bin/ --service should still show service: yes"

    if err_out="$(HOME="$fake_home" "$SCRIPT" --install-dir "$ro_dir" --version v0.0.0 2>&1)"; then
      fail "explicit unwritable --install-dir should exit non-zero"
    fi
    echo "$err_out" | grep -q "no write permission to $ro_dir" \
      || fail "explicit unwritable --install-dir should name the dir: $err_out"
    echo "$err_out" | grep -q 're-run with sudo, or pass --install-dir' \
      || fail "explicit unwritable --install-dir should print the hint: $err_out"
    if echo "$err_out" | grep -q 'download'; then
      fail "the writability check must run before the download: $err_out"
    fi
  fi
  echo "ok: legacy"
fi

# The rest needs fixtures; stop here when only legacy was asked for.
if [[ ${#GROUPS_WANTED[@]} -eq 1 && "${GROUPS_WANTED[0]}" == "legacy" ]]; then
  [[ "$(host_bin_state)" == "$HOST_BIN_BEFORE" ]] \
  || fail "a test changed the host's /usr/local/bin/toolu-runner"
echo "install.sh tests passed"
  exit 0
fi

# =========================================================================
# fixtures: real binary, release dirs, loopback server, a jq-free PATH
# =========================================================================
for tool in python3 setsid script timeout zsh fish sha256sum shasum; do
  command -v "$tool" >/dev/null 2>&1 \
    || fail "$tool is required by the install.sh scenarios (install it; CI installs zsh and fish). This suite is Linux-only (util-linux setsid/script) and needs both sha256sum and shasum to cover both hash paths; on macOS run tests/install_sh_bash32_test.sh, which accepts either."
done

REAL_BIN="${TOOLU_RUNNER_TEST_BIN:-$ROOT/target/debug/toolu-runner}"
if [[ ! -x "$REAL_BIN" && -z "${TOOLU_RUNNER_TEST_BIN:-}" ]]; then
  echo "building toolu-runner (target/debug) for the install.sh scenarios"
  (cd "$ROOT" && cargo build -q -p toolu-runner --bin toolu-runner)
fi
[[ -x "$REAL_BIN" ]] || fail "real toolu-runner binary not found at $REAL_BIN"
WORKSPACE_VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"
[[ "$("$REAL_BIN" --version)" == "toolu-runner $WORKSPACE_VERSION" ]] \
  || fail "built binary is not the workspace version $WORKSPACE_VERSION: $("$REAL_BIN" --version)"

case "$(uname -m)" in
  x86_64|amd64) ARCH=amd64 ;;
  aarch64|arm64) ARCH=arm64 ;;
  *) fail "unsupported test host arch $(uname -m)" ;;
esac
case "$(uname -s)" in
  Linux) OS=linux ;;
  Darwin) OS=darwin ;;
  *) fail "unsupported test host OS $(uname -s)" ;;
esac
ASSET="toolu-runner-$OS-$ARCH.tar.gz"

# A setup-capable stand-in: answers --version and `setup --help`, and records
# what `setup` was exec'd with (argv, stdin, TOOLU_RUNNER_HOME, PATH).
FIXTURE_BIN="$tmp/fixture/toolu-runner"
mkdir -p "$tmp/fixture" "$tmp/logs"
cat >"$FIXTURE_BIN" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --version) echo "toolu-runner 9.9.9" ;;
  setup)
    shift
    if [[ "${1:-}" == "--help" ]]; then echo "usage: toolu-runner setup"; exit 0; fi
    {
      echo "argc=$#"
      for a in "$@"; do echo "arg=$a"; done
      if [[ -t 0 ]]; then echo "stdin=tty"; else echo "stdin=$(readlink /proc/self/fd/0 2>/dev/null || echo other)"; fi
      echo "home=${TOOLU_RUNNER_HOME:-}"
      echo "path=$PATH"
    } >"$FIXTURE_LOG"
    echo "fixture setup ran"
    ;;
  *) echo "fixture: unexpected $*" >&2; exit 2 ;;
esac
EOF
chmod 0755 "$FIXTURE_BIN"

SRV="$tmp/srv"
# make_release <name> <binary>: a release reachable both as latest and as v9.9.9.
make_release() {
  local name="$1" bin="$2" latest="$SRV/$1/latest/download"
  mkdir -p "$latest"
  bash "$ROOT/scripts/package-release.sh" "$OS" "$ARCH" "$bin" "$latest" >/dev/null
  (cd "$latest" && sha256sum -- *.tar.gz >SHA256SUMS)
  mkdir -p "$SRV/$name/download"
  cp -R "$latest" "$SRV/$name/download/v9.9.9"
}
make_release real "$REAL_BIN"
make_release setup "$FIXTURE_BIN"
# tampered: one byte of the tarball flipped AFTER SHA256SUMS was computed.
make_release tampered "$REAL_BIN"
python3 -I - "$SRV/tampered/latest/download/$ASSET" <<'EOF'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
b[len(b) // 2] ^= 0x01
open(p, "wb").write(b)
EOF
# broken: a binary that does not run (stands in for a wrong-arch download or a
# noexec install dir).
mkdir -p "$tmp/broken"
printf '#!/bin/sh\nexit 1\n' >"$tmp/broken/toolu-runner"
chmod 0755 "$tmp/broken/toolu-runner"
make_release broken "$tmp/broken/toolu-runner"
# nosum: SHA256SUMS lists another platform only.
make_release nosum "$REAL_BIN"
awk '{ print $1 "  toolu-runner-plan9-mips.tar.gz" }' "$SRV/nosum/latest/download/SHA256SUMS" \
  >"$SRV/nosum/latest/download/SHA256SUMS.new"
mv "$SRV/nosum/latest/download/SHA256SUMS.new" "$SRV/nosum/latest/download/SHA256SUMS"

python3 -I - "$SRV" "$tmp/port" <<'EOF' &
import http.server, os, sys
root, portfile = sys.argv[1], sys.argv[2]
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=root, **k)
    def log_message(self, *a):
        pass
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
open(portfile + ".tmp", "w").write(str(s.server_address[1]))
os.rename(portfile + ".tmp", portfile)
s.serve_forever()
EOF
SERVER_PID=$!
for _ in $(seq 1 100); do
  [[ -s "$tmp/port" ]] && break
  sleep 0.1
done
[[ -s "$tmp/port" ]] || fail "fixture HTTP server did not start"
BASE="http://127.0.0.1:$(cat "$tmp/port")"

# A PATH of symlinks to exactly the tools an install needs: no jq. The shasum
# variant also drops sha256sum (the macOS shape).
make_path() {
  local dir="$1" skip="$2" t p
  mkdir -p "$dir"
  for t in bash sh env curl tar gzip install mktemp rm rmdir mkdir mv cp chmod cat \
    awk sed grep head tail tr wc cut uname dirname basename ls id readlink \
    sha256sum shasum zsh fish; do
    [[ "$t" == "$skip" ]] && continue
    p="$(command -v "$t")" || fail "tool $t not found for the clean PATH"
    ln -sf "$p" "$dir/$t"
  done
}
CLEAN_PATH="$tmp/path-full"
make_path "$CLEAN_PATH" none
SHASUM_PATH="$tmp/path-shasum"
make_path "$SHASUM_PATH" sha256sum
if PATH="$CLEAN_PATH" command -v jq >/dev/null 2>&1; then fail "clean PATH must not have jq"; fi
if PATH="$SHASUM_PATH" command -v sha256sum >/dev/null 2>&1; then fail "shasum PATH must not have sha256sum"; fi

# run_install <mode> <home> <release> [install.sh args...] -> OUT, RC
#   notty: no controlling terminal (setsid), stdin /dev/null
#   pipe:  no controlling terminal, the script piped on stdin (curl | bash)
#   pty:   under script(1): a pseudo-terminal, the script piped on stdin
# RUN_PATH / RUN_TERM / RUN_ENV (array) tune the environment; RUN_CWD sets the
# working directory (notty only).
RUN_ENV=()
run_install() {
  local mode="$1" home="$2" rel="$3"
  shift 3
  local log
  log="$tmp/logs/$(basename "$home").log"
  rm -f "$log"
  # TOOLU_RUNNER_INSTALL_DIR keeps every install inside the temporary HOME:
  # as root the default /usr/local/bin would be writable and get overwritten.
  local -a envv=(env -i "HOME=$home" "PATH=${RUN_PATH:-$CLEAN_PATH}" "TERM=${RUN_TERM:-dumb}"
    "TOOLU_RUNNER_RELEASE_URL=$BASE/$rel" "FIXTURE_LOG=$log"
    "TOOLU_RUNNER_INSTALL_DIR=$home/.local/bin")
  if [[ ${#RUN_ENV[@]} -gt 0 ]]; then envv+=("${RUN_ENV[@]}"); fi
  RC=0
  case "$mode" in
    notty)
      OUT="$(cd "${RUN_CWD:-.}" && setsid -w timeout 60 "${envv[@]}" bash "$SCRIPT" "$@" </dev/null 2>&1)" || RC=$?
      ;;
    pipe)
      OUT="$(cat "$SCRIPT" | setsid -w timeout 60 "${envv[@]}" bash -s -- "$@" 2>&1)" || RC=$?
      ;;
    pty)
      local cmd q
      cmd="cat $(printf '%q' "$SCRIPT") | $(printf '%q ' "${envv[@]}") bash -s --"
      for q in "$@"; do cmd+=" $(printf '%q' "$q")"; done
      OUT="$(timeout 60 script -qec "$cmd" /dev/null </dev/null 2>&1 | tr -d '\r')" || RC=$?
      ;;
  esac
  LOG="$log"
}
new_home() {
  local h="$tmp/homes/$1"
  rm -rf "$h"
  mkdir -p "$h"
  echo "$h"
}
count_blocks() { grep -c '^# >>> toolu-runner >>>' "$1" || true; }
strip_ansi() { sed $'s/\033\\[[0-9;]*m//g'; }

# =========================================================================
# S1 / S1b: no jq, checksum verified, version from the binary
# =========================================================================
if want S1; then
  echo "S1: install with no jq, version from the binary"
  h="$(new_home s1)"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S1 install should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -qx "toolu-runner: installed $WORKSPACE_VERSION (checksum ok)" \
    || fail "S1 should print the binary's version with checksum ok: $OUT"
  cmp -s "$REAL_BIN" "$h/.local/bin/toolu-runner" || fail "S1 installed binary differs from the release binary"
  for leftover in "$h/.local/bin"/.toolu-runner.new.*; do
    [[ ! -e "$leftover" ]] || fail "S1 left a temporary binary behind: $leftover"
  done
  if echo "$OUT" | grep -qi 'jq'; then fail "S1 output mentions jq: $OUT"; fi
  echo "ok: S1"
fi

if want S1b; then
  echo "S1b: shasum is the only hash tool"
  h="$(new_home s1b)"
  RUN_PATH="$SHASUM_PATH" run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S1b install with shasum only should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -qx "toolu-runner: installed $WORKSPACE_VERSION (checksum ok)" \
    || fail "S1b should verify with shasum: $OUT"
  # Pinned versions verify against that tag's SHA256SUMS.
  h="$(new_home s1b-pinned)"
  run_install notty "$h" real --version v9.9.9 --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S1b pinned install should exit 0 (rc=$RC): $OUT"
  echo "ok: S1b"
fi

# =========================================================================
# S2: a tampered tarball (or a missing checksum) installs nothing
# =========================================================================
if want S2; then
  echo "S2: tampered tarball is refused"
  h="$(new_home s2)"
  run_install notty "$h" tampered --install-dir "$h/.local/bin"
  [[ "$RC" -eq 1 ]] || fail "S2 tampered install should exit 1 (rc=$RC): $OUT"
  echo "$OUT" | grep -qx "toolu-runner: SHA256 mismatch for $ASSET. Nothing was installed." \
    || fail "S2 should name the asset and say nothing was installed: $OUT"
  [[ ! -e "$h/.local/bin/toolu-runner" ]] || fail "S2 must not install the binary"
  [[ -z "$(ls -A "$h/.local/bin" 2>/dev/null)" ]] || fail "S2 left files in the install dir"
  [[ ! -e "$h/.toolu-runner" ]] || fail "S2 must not write ~/.toolu-runner"
  for f in .profile .bashrc .zshenv; do
    [[ ! -e "$h/$f" ]] || fail "S2 must not create $f"
  done

  echo "S2: a tampered tarball on a terminal shows the ✕ line"
  h="$(new_home s2-pty)"
  run_install pty "$h" tampered --install-dir "$h/.local/bin"
  [[ "$RC" -eq 1 ]] || fail "S2 pty tampered install should exit 1 (rc=$RC): $OUT"
  echo "$OUT" | grep -q "✕ SHA256 mismatch for $ASSET." || fail "S2 pty should print the ✕ line: $OUT"
  echo "$OUT" | grep -q "Nothing was installed. Re-run, or report it if it repeats." \
    || fail "S2 pty should say nothing was installed: $OUT"

  echo "S2: SHA256SUMS without this asset is refused"
  h="$(new_home s2-nosum)"
  run_install notty "$h" nosum --install-dir "$h/.local/bin"
  [[ "$RC" -eq 1 ]] || fail "S2 missing checksum should exit 1 (rc=$RC): $OUT"
  echo "$OUT" | grep -q "SHA256SUMS has no entry for $ASSET" || fail "S2 should name the missing entry: $OUT"
  [[ ! -e "$h/.local/bin/toolu-runner" ]] || fail "S2 missing checksum must not install"

  echo "S2: a binary that does not run never replaces the installed one"
  h="$(new_home s2-broken)"
  mkdir -p "$h/.local/bin"
  cp "$REAL_BIN" "$h/.local/bin/toolu-runner"
  run_install notty "$h" broken --install-dir "$h/.local/bin"
  [[ "$RC" -eq 1 ]] || fail "S2 a non-running binary should exit 1 (rc=$RC): $OUT"
  echo "$OUT" | grep -q "does not run here ($OS/$ARCH, or $h/.local/bin is mounted noexec). Nothing was installed." \
    || fail "S2 should say the binary does not run: $OUT"
  cmp -s "$REAL_BIN" "$h/.local/bin/toolu-runner" || fail "S2 the working binary must stay in place"
  for leftover in "$h/.local/bin"/.toolu-runner.new.*; do
    [[ ! -e "$leftover" ]] || fail "S2 left a staged binary behind: $leftover"
  done
  echo "ok: S2"
fi

# =========================================================================
# S3 / S4: environment files, login shells, idempotence, uninstall
# =========================================================================
# seed_dotfiles <home>: pre-existing profiles plus a fish config dir.
seed_dotfiles() {
  local h="$1"
  printf '# user profile\nexport USER_PROFILE=1\n' >"$h/.profile"
  printf '# user bashrc\nalias ll="ls -l"\n' >"$h/.bashrc"
  printf '# user zshenv\nexport USER_ZSHENV=1\n' >"$h/.zshenv"
  mkdir -p "$h/.config/fish"
}
login_finds() { # <home> <shell...>
  local h="$1"
  shift
  env -i "HOME=$h" "PATH=/usr/bin:/bin" TERM=dumb "$@" 2>/dev/null
}

if want S3; then
  echo "S3: env files + one marker block per profile, run twice"
  h="$(new_home s3)"
  seed_dotfiles "$h"
  mkdir -p "$tmp/s3-first"
  for run in 1 2; do
    run_install notty "$h" real --install-dir "$h/.local/bin"
    [[ "$RC" -eq 0 ]] || fail "S3 install should exit 0 (rc=$RC): $OUT"
    for f in .profile .bashrc .zshenv .toolu-runner/env .toolu-runner/env.fish .config/fish/conf.d/toolu-runner.fish; do
      if [[ "$run" -eq 1 ]]; then
        cp "$h/$f" "$tmp/s3-first/$(basename "$f")"
      else
        cmp -s "$tmp/s3-first/$(basename "$f")" "$h/$f" || fail "S3 a second run changed $f"
      fi
    done
  done
  for f in .profile .bashrc .zshenv; do
    [[ "$(count_blocks "$h/$f")" -eq 1 ]] || fail "S3 $f should hold exactly one marker block: $(cat "$h/$f")"
  done
  grep -q '^# user bashrc$' "$h/.bashrc" || fail "S3 must keep the user's .bashrc content"
  [[ -f "$h/.toolu-runner/env" && -f "$h/.toolu-runner/env.fish" ]] || fail "S3 should write env and env.fish"
  [[ -f "$h/.config/fish/conf.d/toolu-runner.fish" ]] || fail "S3 should write the fish conf.d file"
  if grep -q TOOLU_RUNNER_HOME "$h/.toolu-runner/env"; then fail "S3 env must not set TOOLU_RUNNER_HOME without --home"; fi
  want_bin="$h/.local/bin/toolu-runner"
  got="$(login_finds "$h" bash -lc 'command -v toolu-runner')" || true
  [[ "$got" == "$want_bin" ]] || fail "S3 bash -lc should find $want_bin, got '$got'"
  got="$(login_finds "$h" zsh -lc 'command -v toolu-runner')" || true
  [[ "$got" == "$want_bin" ]] || fail "S3 zsh -lc should find $want_bin, got '$got'"
  got="$(login_finds "$h" fish -lc 'command -v toolu-runner')" || true
  [[ "$got" == "$want_bin" ]] || fail "S3 fish -lc should find $want_bin, got '$got'"
  # Interactive bash (reads .bashrc only) finds it too.
  got="$(login_finds "$h" bash -ic 'command -v toolu-runner')" || true
  [[ "$got" == "$want_bin" ]] || fail "S3 bash -ic should find $want_bin, got '$got'"

  echo "S3: ~/.bash_profile shadows ~/.profile, so it gets the block too"
  h="$(new_home s3-bash-profile)"
  printf '# user bash_profile\n' >"$h/.bash_profile"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S3 bash_profile install should exit 0 (rc=$RC): $OUT"
  [[ "$(count_blocks "$h/.bash_profile")" -eq 1 ]] || fail "S3 .bash_profile should get the block"
  got="$(login_finds "$h" bash -lc 'command -v toolu-runner')" || true
  [[ "$got" == "$h/.local/bin/toolu-runner" ]] || fail "S3 bash -lc with .bash_profile should find it, got '$got'"

  echo "S3: the PATH row on a terminal names the files it changed"
  h="$(new_home s3-pty)"
  RUN_TERM=xterm-256color run_install pty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S3 pty install should exit 0 (rc=$RC): $OUT"
  plain="$(echo "$OUT" | strip_ansi)"
  echo "$plain" | grep -q '✓ PATH         added to ~/.profile, ~/.bashrc, ~/.zshenv and fish' \
    || fail "S3 pty PATH row should list the files: $plain"

  echo "S3: ZDOTDIR and XDG_CONFIG_HOME are honoured"
  h="$(new_home s3-xdg)"
  RUN_ENV=("ZDOTDIR=$h/zdot" "XDG_CONFIG_HOME=$h/xdg")
  run_install notty "$h" real --install-dir "$h/.local/bin"
  RUN_ENV=()
  [[ "$RC" -eq 0 ]] || fail "S3 ZDOTDIR install should exit 0 (rc=$RC): $OUT"
  [[ "$(count_blocks "$h/zdot/.zshenv")" -eq 1 ]] || fail "S3 \$ZDOTDIR/.zshenv should get the block"
  [[ ! -e "$h/.zshenv" ]] || fail "S3 with ZDOTDIR set, ~/.zshenv must not be written"
  [[ -f "$h/xdg/fish/conf.d/toolu-runner.fish" ]] || fail "S3 the fish file should go under XDG_CONFIG_HOME"
  got="$(login_finds "$h" "ZDOTDIR=$h/zdot" zsh -lc 'command -v toolu-runner')" || true
  [[ "$got" == "$h/.local/bin/toolu-runner" ]] || fail "S3 zsh with ZDOTDIR should find it, got '$got'"
  got="$(login_finds "$h" "XDG_CONFIG_HOME=$h/xdg" fish -lc 'command -v toolu-runner')" || true
  [[ "$got" == "$h/.local/bin/toolu-runner" ]] || fail "S3 fish with XDG_CONFIG_HOME should find it, got '$got'"
  echo "ok: S3"
fi

if want S4; then
  echo "S4: --uninstall removes blocks and env files, profiles byte-identical"
  h="$(new_home s4)"
  seed_dotfiles "$h"
  # No trailing newline: the block must still come off byte for byte.
  printf '# user bash_login, no final newline' >"$h/.bash_login"
  # An unmatched marker (a hand edit) is left alone by install and uninstall.
  printf '# user bash_profile\n# >>> toolu-runner >>>\necho half a block\n' >"$h/.bash_profile"
  mkdir -p "$tmp/s4-snap"
  for f in .profile .bashrc .zshenv .bash_login .bash_profile; do cp "$h/$f" "$tmp/s4-snap/$f"; done
  run_install notty "$h" real --install-dir "$h/.local/bin"
  run_install notty "$h" real --install-dir "$h/.local/bin" --home "$tmp/s4-runner-home"
  [[ "$RC" -eq 0 ]] || fail "S4 install should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -qx 'install.sh: ~/.bash_profile has an unmatched toolu-runner marker; left unchanged' \
    || fail "S4 install should warn about the unmatched marker: $OUT"
  cmp -s "$tmp/s4-snap/.bash_profile" "$h/.bash_profile" || fail "S4 install must not touch a file with an unmatched marker"
  for f in .profile .bashrc .zshenv .bash_login; do
    [[ "$(count_blocks "$h/$f")" -eq 1 ]] || fail "S4 $f should hold one block before uninstall"
  done
  run_install notty "$h" real --uninstall
  [[ "$RC" -eq 0 ]] || fail "S4 --uninstall should exit 0 (rc=$RC): $OUT"
  for f in .profile .bashrc .zshenv .bash_login .bash_profile; do
    cmp -s "$tmp/s4-snap/$f" "$h/$f" || fail "S4 $f should be byte-identical after --uninstall: $(od -c "$h/$f" | tail -3)"
  done
  echo "$OUT" | grep -q "left unchanged (unmatched toolu-runner marker; edit by hand): ~/.bash_profile" \
    || fail "S4 --uninstall should name the file it left alone: $OUT"
  for f in .toolu-runner/env .toolu-runner/env.fish .config/fish/conf.d/toolu-runner.fish; do
    [[ ! -e "$h/$f" ]] || fail "S4 --uninstall should remove $f"
  done
  [[ -x "$h/.local/bin/toolu-runner" ]] || fail "S4 --uninstall must leave the binary in place"
  echo "$OUT" | grep -q 'removed' || fail "S4 --uninstall should say what it removed: $OUT"
  run_install notty "$h" real --uninstall
  [[ "$RC" -eq 0 ]] || fail "S4 a second --uninstall should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -q 'nothing to remove' || fail "S4 second --uninstall should say nothing to remove: $OUT"

  echo "S4: text appended after a block keeps its own line on --uninstall"
  h="$(new_home s4-append)"
  printf 'export A=1' >"$h/.bashrc"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S4 append install should exit 0 (rc=$RC): $OUT"
  printf 'export B=2\n' >>"$h/.bashrc"
  # A re-run strips and re-adds the block; the user's lines must survive it.
  printf '# >>> toolu-runner >>>\nold line\n# <<< toolu-runner <<<\n' >>"$h/.profile"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S4 append re-run should exit 0 (rc=$RC): $OUT"
  run_install notty "$h" real --uninstall
  [[ "$RC" -eq 0 ]] || fail "S4 append --uninstall should exit 0 (rc=$RC): $OUT"
  [[ "$(cat "$h/.bashrc"; printf x)" == $'export A=1\nexport B=2\nx' ]] \
    || fail "S4 appended text must stay on its own line: $(od -c "$h/.bashrc" | head -3)"

  echo "S4: files the installer created are removed again"
  h="$(new_home s4-created)"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ -f "$h/.profile" && -f "$h/.bashrc" && -f "$h/.zshenv" ]] || fail "S4 install should create the profiles"
  run_install notty "$h" real --uninstall
  [[ "$RC" -eq 0 ]] || fail "S4 --uninstall should exit 0 (rc=$RC): $OUT"
  for f in .profile .bashrc .zshenv .toolu-runner .config/fish/conf.d; do
    [[ ! -e "$h/$f" ]] || fail "S4 --uninstall should remove the created $f"
  done
  echo "ok: S4"
fi

# =========================================================================
# S5 / S5b: no terminal and --no-setup never block or run setup
# =========================================================================
if want S5; then
  echo "S5: curl | bash with no terminal and no flags"
  h="$(new_home s5)"
  run_install pipe "$h" setup
  [[ "$RC" -eq 0 ]] || fail "S5 should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -qx 'toolu-runner: installed 9.9.9 (checksum ok)' || fail "S5 should print the plain line: $OUT"
  echo "$OUT" | grep -q '^next: toolu-runner setup' || fail "S5 should print the next line: $OUT"
  [[ ! -e "$LOG" ]] || fail "S5 must not run setup without a terminal or flags"
  echo "ok: S5"
fi

if want S5b; then
  echo "S5b: bash -s -- --no-setup on a terminal"
  h="$(new_home s5b)"
  run_install pty "$h" setup --no-setup
  [[ "$RC" -eq 0 ]] || fail "S5b should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -q '→ next: toolu-runner setup' || fail "S5b should print the next row: $OUT"
  [[ ! -e "$LOG" ]] || fail "S5b --no-setup must not run setup"
  echo "ok: S5b"
fi

# =========================================================================
# S6 / S6b: handoff to setup
# =========================================================================
if want S6; then
  echo "S6: curl | bash on a terminal hands the tty to setup"
  h="$(new_home s6)"
  RUN_TERM=xterm-256color run_install pty "$h" setup --source github --org acme --yes
  [[ "$RC" -eq 0 ]] || fail "S6 should exit with setup's status 0 (rc=$RC): $OUT"
  [[ -f "$LOG" ]] || fail "S6 setup was not exec'd: $OUT"
  [[ "$(grep '^arg=' "$LOG" | tr '\n' ' ')" == "arg=--source arg=github arg=--org arg=acme arg=--yes " ]] \
    || fail "S6 setup argv wrong: $(cat "$LOG")"
  grep -qx 'stdin=tty' "$LOG" || fail "S6 setup stdin should be the terminal: $(cat "$LOG")"
  grep -q "^path=$h/.local/bin:" "$LOG" || fail "S6 setup PATH should start with the install dir: $(cat "$LOG")"
  echo "$OUT" | grep -q $'\033\\[' || fail "S6 a colour terminal should get colour"
  plain="$(echo "$OUT" | strip_ansi)"
  echo "$plain" | grep -q '^  toolu-runner installer$' || fail "S6 header missing: $plain"
  echo "$plain" | grep -Eq "^  ✓ Downloaded   toolu-runner 9\\.9\\.9 · $OS-$ARCH · [0-9.]+ MB$" \
    || fail "S6 Downloaded row wrong: $plain"
  echo "$plain" | grep -q '^  ✓ Verified     SHA256SUMS match$' || fail "S6 Verified row missing: $plain"
  echo "$plain" | grep -q '^  ✓ Installed    ~/.local/bin/toolu-runner$' || fail "S6 Installed row missing: $plain"
  echo "$plain" | grep -q '^  ✓ PATH         added to ' || fail "S6 PATH row missing: $plain"
  echo "$plain" | grep -q '^  → Starting setup…$' || fail "S6 should announce setup: $plain"
  echo "$plain" | grep -q '^fixture setup ran$' || fail "S6 setup output missing: $plain"

  echo "S6: NO_COLOR keeps the rows but drops colour"
  h="$(new_home s6-nocolor)"
  RUN_ENV=(NO_COLOR=1)
  RUN_TERM=xterm-256color run_install pty "$h" setup --no-setup
  RUN_ENV=()
  [[ "$RC" -eq 0 ]] || fail "S6 NO_COLOR should exit 0 (rc=$RC): $OUT"
  if echo "$OUT" | grep -q $'\033\\['; then fail "S6 NO_COLOR output has escape codes: $OUT"; fi
  echo "$OUT" | grep -q '✓ Verified' || fail "S6 NO_COLOR should keep the ✓ rows: $OUT"
  echo "ok: S6"
fi

if want S6b; then
  echo "S6b: no terminal + setup flags execs setup with exactly those flags"
  h="$(new_home s6b)"
  run_install pipe "$h" setup --home "$tmp/s6b-home" --source github --no-modify-path \
    --repo acme/web -- --yes --name "vps 1"
  [[ "$RC" -eq 0 ]] || fail "S6b should exit 0 (rc=$RC): $OUT"
  [[ -f "$LOG" ]] || fail "S6b setup was not exec'd: $OUT"
  [[ "$(grep -c '^arg=' "$LOG")" -eq 7 ]] || fail "S6b setup should get 7 args: $(cat "$LOG")"
  [[ "$(grep '^arg=' "$LOG" | tr '\n' '|')" == "arg=--source|arg=github|arg=--repo|arg=acme/web|arg=--yes|arg=--name|arg=vps 1|" ]] \
    || fail "S6b setup argv wrong: $(cat "$LOG")"
  grep -qx 'stdin=/dev/null' "$LOG" || fail "S6b setup stdin must be /dev/null, never the script: $(cat "$LOG")"
  grep -qx "home=$tmp/s6b-home" "$LOG" || fail "S6b setup should get TOOLU_RUNNER_HOME: $(cat "$LOG")"
  echo "$OUT" | grep -qx 'toolu-runner: installed 9.9.9 (checksum ok)' || fail "S6b plain line missing: $OUT"
  [[ ! -e "$h/.toolu-runner/env" ]] || fail "S6b --no-modify-path must not write env files"

  echo "S6b: --toolu is handed to setup as --enroll-token"
  h="$(new_home s6b-toolu)"
  enroll_tok="toolu-test-enroll-6c2e"
  run_install pipe "$h" setup --no-modify-path --toolu "$enroll_tok"
  [[ "$RC" -eq 0 ]] || fail "S6b --toolu should exit 0 (rc=$RC): $OUT"
  [[ "$(grep '^arg=' "$LOG" | tr '\n' '|')" == "arg=--enroll-token|arg=$enroll_tok|" ]] \
    || fail "S6b --toolu argv wrong: $(cat "$LOG")"
  if echo "$OUT" | grep -q -- "$enroll_tok"; then fail "S6b installer output echoed the token: $OUT"; fi
  if grep -R -q -- "$enroll_tok" "$h" 2>/dev/null; then
    fail "S6b the enrollment token was written under HOME"
  fi
  echo "ok: S6b"
fi

# =========================================================================
# S7: no secret is written to an env or profile file
# =========================================================================
if want S7; then
  echo "S7: secrets never reach env or profile files"
  h="$(new_home s7)"
  seed_dotfiles "$h"
  env_secret="toolu-test-secret-env-3f9a"
  arg_secret="toolu-test-secret-arg-77c1"
  RUN_ENV=("TOOLU_RUNNER_TOKEN=$env_secret")
  run_install pipe "$h" setup \
    --home "$tmp/s7-home" --source github --repo acme/web --auth token --token "$arg_secret" --yes
  RUN_ENV=()
  [[ "$RC" -eq 0 ]] || fail "S7 should exit 0 (rc=$RC): $OUT"
  grep -q "arg=$arg_secret" "$LOG" || fail "S7 setup should still receive --token: $(cat "$LOG")"
  [[ -f "$h/.toolu-runner/env" ]] || fail "S7 should have written env files"
  if grep -rl -e "$env_secret" -e "$arg_secret" "$h" >/dev/null 2>&1; then
    fail "S7 a secret was written under HOME: $(grep -rl -e "$env_secret" -e "$arg_secret" "$h")"
  fi
  if grep -q 'TOOLU_RUNNER_TOKEN' "$h/.toolu-runner/env" "$h/.toolu-runner/env.fish"; then
    fail "S7 env files must not mention TOOLU_RUNNER_TOKEN"
  fi

  echo "S7: a GitHub setup that also gets --enroll-token is refused without echoing tokens"
  h="$(new_home s7-echo)"
  run_install notty "$h" real --source github --token "$arg_secret" --enroll-token="$env_secret" --yes
  [[ "$RC" -eq 2 ]] || fail "S7 a conflicting setup option should exit 2 (rc=$RC): $OUT"
  if echo "$OUT" | grep -q -e "$arg_secret" -e "$env_secret"; then fail "S7 a token value was echoed: $OUT"; fi
  echo "$OUT" | grep -qF -- "--enroll-token pairs with toolu.sh" \
    || fail "S7 the refusal should name --enroll-token, not its value: $OUT"
  out_check="$("$SCRIPT" --check --token "$arg_secret")"
  if echo "$out_check" | grep -q -- "$arg_secret"; then fail "S7 --check echoed a token: $out_check"; fi
  run_install notty "$h" real --no-setup --token "$arg_secret"
  if echo "$OUT" | grep -q -- "$arg_secret"; then fail "S7 a usage error echoed a token: $OUT"; fi
  echo "ok: S7"
fi

# =========================================================================
# S8 / S8b: the real binary's setup, conflicting flags
# =========================================================================
if want S8; then
  echo "S8: setup flags exec the real setup, which refuses unattended"
  h="$(new_home s8)"
  run_install notty "$h" real --install-dir "$h/.local/bin" --source github --org acme
  [[ "$RC" -eq 2 ]] || fail "S8 should exit 2 (rc=$RC): $OUT"
  if echo "$OUT" | grep -q "has no setup command"; then fail "S8 the binary has setup now: $OUT"; fi
  echo "$OUT" | grep -q "setup: machine" || fail "S8 should show setup's machine check: $OUT"
  [[ -x "$h/.local/bin/toolu-runner" ]] || fail "S8 the binary should stay installed"

  echo "S8: conflicting flags fail before any download"
  h="$(new_home s8-conflict)"
  run_install notty "$h" real --install-dir "$h/.local/bin" --no-setup --org acme
  [[ "$RC" -eq 2 ]] || fail "S8 --no-setup with setup flags should exit 2 (rc=$RC): $OUT"
  echo "$OUT" | grep -q -- "--no-setup" || fail "S8 conflict should name --no-setup: $OUT"
  run_install notty "$h" real --uninstall --org acme
  [[ "$RC" -eq 2 ]] || fail "S8 --uninstall with other options should exit 2 (rc=$RC): $OUT"
  run_install notty "$h" real --home relative/dir
  [[ "$RC" -eq 2 ]] || fail "S8 a relative --home should exit 2 (rc=$RC): $OUT"
  [[ ! -e "$h/.local/bin/toolu-runner" ]] || fail "S8 usage errors must not install anything"
  echo "ok: S8"
fi

if want S8b; then
  echo "S8b: the real binary with no flags points at setup"
  h="$(new_home s8b)"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S8b should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -q '^next: toolu-runner setup' || fail "S8b should point at setup: $OUT"
  if echo "$OUT" | grep -q -- '--token'; then fail "S8b next line must not ask for a token flag: $OUT"; fi
  echo "ok: S8b"
fi

# =========================================================================
# S10: install dir already on PATH, --home, --no-modify-path
# =========================================================================
if want S10; then
  echo "S10: dir already on PATH and no --home writes nothing"
  h="$(new_home s10)"
  mkdir -p "$h/.local/bin"
  RUN_PATH="$h/.local/bin:$CLEAN_PATH" run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S10 should exit 0 (rc=$RC): $OUT"
  [[ ! -e "$h/.toolu-runner" && ! -e "$h/.profile" && ! -e "$h/.bashrc" ]] \
    || fail "S10 must not write env or profile files when the dir is on PATH"

  echo "S10: dir on PATH with --home writes a home-only env"
  h="$(new_home s10-home)"
  mkdir -p "$h/.local/bin"
  RUN_PATH="$h/.local/bin:$CLEAN_PATH" run_install notty "$h" real --install-dir "$h/.local/bin" \
    --home "$tmp/s10-runner-home"
  [[ "$RC" -eq 0 ]] || fail "S10 --home should exit 0 (rc=$RC): $OUT"
  grep -qx "export TOOLU_RUNNER_HOME=\"$tmp/s10-runner-home\"" "$h/.toolu-runner/env" \
    || fail "S10 env should export TOOLU_RUNNER_HOME: $(cat "$h/.toolu-runner/env")"
  if grep -q 'PATH=' "$h/.toolu-runner/env"; then fail "S10 home-only env must not touch PATH"; fi
  [[ "$(count_blocks "$h/.profile")" -eq 1 ]] || fail "S10 --home should still source env from .profile"
  got="$(login_finds "$h" bash -lc 'echo "$TOOLU_RUNNER_HOME"')" || true
  [[ "$got" == "$tmp/s10-runner-home" ]] || fail "S10 a login shell should see TOOLU_RUNNER_HOME, got '$got'"

  echo "S10: --no-modify-path writes nothing and prints the export line"
  h="$(new_home s10-nomod)"
  run_install notty "$h" real --install-dir "$h/.local/bin" --no-modify-path
  [[ "$RC" -eq 0 ]] || fail "S10 --no-modify-path should exit 0 (rc=$RC): $OUT"
  [[ ! -e "$h/.toolu-runner" && ! -e "$h/.profile" ]] || fail "S10 --no-modify-path must write nothing"
  echo "$OUT" | grep -qF "export PATH=\"$h/.local/bin:\$PATH\"" || fail "S10 should print the export line: $OUT"
  echo "$OUT" | grep -q "^next: $h/.local/bin/toolu-runner setup" \
    || fail "S10 next line should use the full path when it is not on PATH: $OUT"

  echo "S10: an unwritable ~/.toolu-runner is a warning, not a failed install"
  h="$(new_home s10-envfile)"
  : >"$h/.toolu-runner"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S10 an unwritable env dir should still exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -qx "toolu-runner: installed $WORKSPACE_VERSION (checksum ok)" || fail "S10 should report the install: $OUT"
  echo "$OUT" | grep -q 'could not write ~/.toolu-runner/env' || fail "S10 should warn about the env file: $OUT"
  echo "$OUT" | grep -qF "export PATH=\"$h/.local/bin:\$PATH\"" || fail "S10 should print the export line: $OUT"
  echo "$OUT" | grep -q "^next: $h/.local/bin/toolu-runner setup" || fail "S10 next line should use the full path: $OUT"
  [[ ! -e "$h/.profile" ]] || fail "S10 must not source an env file it could not write"
  run_install notty "$h" real --install-dir "$h/.local/bin" --home "$tmp/s10-envfile-home"
  [[ "$RC" -eq 0 ]] || fail "S10 an unwritable env dir with --home should exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -qxF "toolu-runner: PATH not changed; add to your shell profile: export PATH=\"$h/.local/bin:\$PATH\"" \
    || fail "S10 the PATH line should stand alone: $OUT"
  echo "$OUT" | grep -qxF "toolu-runner: PATH not changed; add to your shell profile: export TOOLU_RUNNER_HOME=\"$tmp/s10-envfile-home\"" \
    || fail "S10 the TOOLU_RUNNER_HOME line should stand alone: $OUT"

  echo "S10: an unwritable fish config dir only skips fish"
  h="$(new_home s10-fishfile)"
  mkdir -p "$h/.config"
  : >"$h/.config/fish"
  run_install notty "$h" real --install-dir "$h/.local/bin"
  [[ "$RC" -eq 0 ]] || fail "S10 an unwritable fish dir should still exit 0 (rc=$RC): $OUT"
  echo "$OUT" | grep -q 'could not write ~/.config/fish/conf.d/toolu-runner.fish' || fail "S10 should warn about fish: $OUT"
  [[ "$(count_blocks "$h/.profile")" -eq 1 ]] || fail "S10 the other profiles should still be set up"

  # Root reads any file, so the unreadable-profile case needs a normal user.
  if [[ "$(id -u)" -ne 0 ]]; then
    echo "S10: an unreadable profile is never overwritten"
    h="$(new_home s10-unreadable)"
    printf '# private bashrc\nexport SECRET_ALIAS=1\n' >"$h/.bashrc"
    cp "$h/.bashrc" "$tmp/s10-bashrc.orig"
    chmod 0200 "$h/.bashrc"
    run_install notty "$h" real --install-dir "$h/.local/bin"
    chmod 0600 "$h/.bashrc"
    [[ "$RC" -eq 0 ]] || fail "S10 an unreadable profile should not fail the install (rc=$RC): $OUT"
    echo "$OUT" | grep -qx 'install.sh: could not read ~/.bashrc; left unchanged' \
      || fail "S10 should warn about the unreadable profile: $OUT"
    cmp -s "$tmp/s10-bashrc.orig" "$h/.bashrc" || fail "S10 an unreadable profile was overwritten: $(cat "$h/.bashrc")"
    [[ "$(count_blocks "$h/.profile")" -eq 1 ]] || fail "S10 the readable profiles should still be set up"
  fi

  echo "S10: a relative --install-dir is written as an absolute path"
  h="$(new_home s10-rel)"
  RUN_CWD="$h" run_install notty "$h" real --install-dir rel-bin
  [[ "$RC" -eq 0 ]] || fail "S10 relative install should exit 0 (rc=$RC): $OUT"
  [[ -x "$h/rel-bin/toolu-runner" ]] || fail "S10 relative install dir should resolve against the cwd"
  grep -qF "export PATH=\"$h/rel-bin:\$PATH\"" "$h/.toolu-runner/env" \
    || fail "S10 env should hold the absolute dir: $(cat "$h/.toolu-runner/env")"
  echo "ok: S10"
fi

[[ "$(host_bin_state)" == "$HOST_BIN_BEFORE" ]] \
  || fail "a test changed the host's /usr/local/bin/toolu-runner"
echo "install.sh tests passed"
