#!/usr/bin/env bash
# install.sh — install toolu-runner from GitHub releases.
#
#   - detect arch (x86_64 / aarch64 / arm64) and OS (darwin / linux)
#   - download the matching release tarball and that release's SHA256SUMS
#     (releases/latest/download/<asset>: no API call, no jq)
#   - verify the tarball against SHA256SUMS; a mismatch installs nothing
#   - install to /usr/local/bin/toolu-runner (or --install-dir); when
#     /usr/local/bin is not writable (Apple Silicon macOS, non-root Linux)
#     and no --install-dir was given, fall back to ~/.local/bin
#   - put the install dir on PATH for new shells: ~/.toolu-runner/env (and
#     env.fish), sourced from the shell profiles by one marker block each
#   - hand the terminal to `toolu-runner setup`
#   - optionally install + start the static service unit (--service)
#
# Usage:
#   curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash
#   curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash -s -- --no-setup
#   curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash -s -- <setup flags>
#   bash install.sh --check                   # print the install plan, no download
#   bash install.sh --uninstall               # undo the shell-profile changes
#   bash install.sh --help                    # show all options
#
# Requires: bash 3.2+, curl, tar, install, awk, and sha256sum or shasum.
set -euo pipefail

REPO="${TOOLU_RUNNER_REPO:-Falconiere/toolu-ghrunner}"
RELEASE_BASE="${TOOLU_RUNNER_RELEASE_URL:-https://github.com/$REPO/releases}"
RELEASE_BASE="${RELEASE_BASE%/}"
BIN_NAME="toolu-runner"

DEFAULT_INSTALL_DIR="${TOOLU_RUNNER_INSTALL_DIR:-/usr/local/bin}"
# Set lazily: HOME may be unset (set -u) in a bare container; then there is
# no fallback and the default is used as is.
FALLBACK_INSTALL_DIR=""
# The static units under scripts/ hardcode this path (see install_service).
STATIC_UNIT_BIN="/usr/local/bin/$BIN_NAME"
INSTALL_DIR=""
INSTALL_DIR_EXPLICIT=0
INSTALL_DIR_FALLBACK=0
VERSION=""
SERVICE=0
CHECK=0
NO_SETUP=0
NO_MODIFY_PATH=0
UNINSTALL=0
RUNNER_HOME=""
# Every argument that is not an installer option, in order, for `setup`.
SETUP_ARGS=()
# Installer options other than --uninstall (which takes none).
OTHER_OPTS=0

# Shell-profile marker block. The line sources the env file only while it
# exists, so a removed env file never breaks a shell.
PROFILE_BEGIN='# >>> toolu-runner >>>'
PROFILE_END='# <<< toolu-runner <<<'
PROFILE_LINE='if [ -f "$HOME/.toolu-runner/env" ]; then . "$HOME/.toolu-runner/env"; fi'

usage() {
  cat <<EOF
install.sh — install $BIN_NAME from GitHub releases, then run "$BIN_NAME setup"

USAGE:
  install.sh [OPTIONS] [SETUP OPTIONS...] [-- SETUP OPTIONS...]

OPTIONS:
  --version <v>        Install a specific version (e.g. v0.1.0). Default: latest.
  --install-dir <dir>  Directory to install the binary into. Default: /usr/local/bin
                       when writable, else \$HOME/.local/bin. An explicit dir is
                       used as given, no fallback.
  --no-modify-path     Do not write ~/.toolu-runner/env or touch shell profiles;
                       print the PATH line to add instead.
  --home <dir>         Runner home (absolute path) for setup and for new shells
                       (exported as TOOLU_RUNNER_HOME). Default: ~/.toolu-runner.
  --no-setup           Install only; do not run "$BIN_NAME setup".
  --uninstall          Remove what this script wrote to shell profiles
                       (marker blocks, ~/.toolu-runner/env*, the fish conf.d file).
                       The binary and runner state are left in place.
  --service            Also install + start the static service unit (launchd plist on
                       macOS, systemd unit on Linux) shipped in the release tarball
                       at scripts/. Those units hardcode $STATIC_UNIT_BIN, so
                       the unit is skipped (with a warning) when the binary lands
                       anywhere else — use "$BIN_NAME install-service" instead.
  --check              Print the install plan and exit. No download, no install.
  --help, -h           Show this message and exit.

Every other option is passed to "$BIN_NAME setup", in order. With a terminal,
setup runs interactively (it reads /dev/tty). Without one, setup runs only when
setup options were given; otherwise the next command is printed.

The download is verified against the release's SHA256SUMS (sha256sum, or
shasum -a 256); a mismatch installs nothing. When the install dir is not on
PATH, ~/.toolu-runner/env (and env.fish) put it there, sourced from ~/.profile,
~/.bashrc (~/.bash_profile, ~/.bash_login when present), ~/.zshenv when zsh is
installed, and the fish conf.d dir. These files never hold secrets.

ENVIRONMENT:
  TOOLU_RUNNER_REPO    Override the GitHub owner/repo. Default: $REPO.
  TOOLU_RUNNER_INSTALL_DIR
                       Preferred install dir when --install-dir is absent. Still
                       falls back to \$HOME/.local/bin when not writable.
  TOOLU_RUNNER_RELEASE_URL
                       Release base URL. Default: https://github.com/\$TOOLU_RUNNER_REPO/releases.
  TOOLU_RUNNER_TOKEN   Read by unattended setup (never written by this script;
                       stdin is the script under curl | bash, so no --token-stdin).
  NO_COLOR             Disable colour.

EXAMPLES:
  # Install the latest release and start setup.
  curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash

  # Install only.
  curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash -s -- --no-setup

  # Unattended (cloud-init): flags go to setup, the token through the env.
  curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | TOOLU_RUNNER_TOKEN="\$PAT" \\
    bash -s -- --source github --repo acme/web --auth token --yes

  # Force /usr/local/bin on a host where it needs root.
  curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | sudo bash

  # Print the plan without downloading.
  bash install.sh --check
EOF
}

err() {
  echo "install.sh: $*" >&2
}

die() {
  err "$@"
  exit 2
}

# Arguments as one shell-like line for messages. The value of a secret-bearing
# option (--token X, --token=X, --enroll-token X) shows as ••••: these lines
# end up in terminals and cloud-init logs.
join_args() {
  local out="" a hide=0
  for a in "$@"; do
    if [[ "$hide" -eq 1 ]]; then
      a="••••"
      hide=0
    else
      case "$a" in
        --token|--enroll-token) hide=1 ;;
        --token=*|--enroll-token=*) a="${a%%=*}=••••" ;;
      esac
    fi
    out="${out:+$out }$a"
  done
  printf '%s' "$out"
}

# --- arg parsing -----------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      [[ $# -ge 2 ]] || die "--version requires a value"
      VERSION="$2"
      OTHER_OPTS=1
      shift 2
      ;;
    --install-dir)
      [[ $# -ge 2 ]] || die "--install-dir requires a value"
      # Strip trailing slashes so the static-unit path comparison below and
      # the printed paths stay canonical (but keep a bare "/").
      INSTALL_DIR="$2"
      while [[ "$INSTALL_DIR" == */ && "$INSTALL_DIR" != "/" ]]; do
        INSTALL_DIR="${INSTALL_DIR%/}"
      done
      INSTALL_DIR_EXPLICIT=1
      OTHER_OPTS=1
      shift 2
      ;;
    --home)
      [[ $# -ge 2 ]] || die "--home requires a value"
      RUNNER_HOME="$2"
      OTHER_OPTS=1
      shift 2
      ;;
    --service)
      SERVICE=1
      OTHER_OPTS=1
      shift
      ;;
    --check)
      CHECK=1
      OTHER_OPTS=1
      shift
      ;;
    --no-setup)
      NO_SETUP=1
      OTHER_OPTS=1
      shift
      ;;
    --no-modify-path)
      NO_MODIFY_PATH=1
      OTHER_OPTS=1
      shift
      ;;
    --uninstall)
      UNINSTALL=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    --)
      shift
      SETUP_ARGS+=("$@")
      break
      ;;
    *)
      SETUP_ARGS+=("$1")
      shift
      ;;
  esac
done

# A value written into a shell file between double quotes must not be able to
# end the quotes or expand.
unsafe_for_shell_file() {
  case "$1" in
    *[\"\\\$\`]* | *$'\n'*) return 0 ;;
  esac
  return 1
}

if [[ "$UNINSTALL" -eq 1 ]] && [[ "$OTHER_OPTS" -eq 1 || ${#SETUP_ARGS[@]} -gt 0 ]]; then
  die "--uninstall takes no other options"
fi
if [[ "$NO_SETUP" -eq 1 && ${#SETUP_ARGS[@]} -gt 0 ]]; then
  die "--no-setup cannot be combined with setup options: $(join_args "${SETUP_ARGS[@]}")"
fi
if [[ -n "$RUNNER_HOME" ]]; then
  [[ "$RUNNER_HOME" == /* ]] || die "--home must be an absolute path (got '$RUNNER_HOME')"
  if unsafe_for_shell_file "$RUNNER_HOME"; then
    die "--home must not contain quotes, backslashes, \$, backticks or newlines"
  fi
  while [[ "$RUNNER_HOME" == */ && "$RUNNER_HOME" != "/" ]]; do
    RUNNER_HOME="${RUNNER_HOME%/}"
  done
fi

# --- output ------------------------------------------------------------------

# Rows and colour on a terminal; one plain line per step otherwise.
PRETTY=0
if [[ -t 1 ]]; then PRETTY=1; fi
C_MUTED="" C_ACCENT="" C_WARN="" C_ERROR="" C_RESET=""
if [[ "$PRETTY" -eq 1 && -z "${NO_COLOR:-}" && "${TERM:-dumb}" != "dumb" ]]; then
  C_MUTED=$'\033[38;5;245m'
  C_ACCENT=$'\033[38;5;149m'
  C_WARN=$'\033[38;5;178m'
  C_ERROR=$'\033[38;5;167m'
  C_RESET=$'\033[0m'
fi

# row <glyph> <colour> <label> <text>
row() {
  printf '  %s%s%s %-12s %s\n' "$2" "$1" "$C_RESET" "$3" "$4"
}

# A path with the home dir shown as ~.
tilde() {
  local home="${HOME:-}"
  if [[ -n "$home" && "$1" == "$home"/* ]]; then
    printf '~%s' "${1#"$home"}"
  else
    printf '%s' "$1"
  fi
}

# "a", "a and b", "a, b and c"
join_and() {
  local n=$# out="" i=0 a
  for a in "$@"; do
    i=$((i + 1))
    if [[ $i -eq 1 ]]; then
      out="$a"
    elif [[ $i -eq $n ]]; then
      out="$out and $a"
    else
      out="$out, $a"
    fi
  done
  printf '%s' "$out"
}

# fail_install <what went wrong> [advice]: exit 1 before anything is installed.
fail_install() {
  if [[ "$PRETTY" -eq 1 ]]; then
    printf '  %s✕%s %s\n' "$C_ERROR" "$C_RESET" "$1" >&2
    printf '    Nothing was installed.%s\n' "${2:+ $2}" >&2
  else
    echo "$BIN_NAME: $1 Nothing was installed." >&2
  fi
  exit 1
}

# --- shell profile files -------------------------------------------------------

ENV_DIR=""
FISH_CONF=""
if [[ -n "${HOME:-}" ]]; then
  ENV_DIR="$HOME/.toolu-runner"
  FISH_CONF="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/$BIN_NAME.fish"
fi

# Profiles to add the block to: ~/.profile and ~/.bashrc always (created when
# absent); ~/.bash_profile / ~/.bash_login only when present, since a bash
# login shell reads the first of them INSTEAD of ~/.profile; ~/.zshenv when
# zsh is installed or the file exists.
profile_targets() {
  local zdot="${ZDOTDIR:-$HOME}"
  echo "$HOME/.profile"
  echo "$HOME/.bashrc"
  if [[ -e "$HOME/.bash_profile" ]]; then echo "$HOME/.bash_profile"; fi
  if [[ -e "$HOME/.bash_login" ]]; then echo "$HOME/.bash_login"; fi
  if command -v zsh >/dev/null 2>&1 || [[ -e "$zdot/.zshenv" ]]; then echo "$zdot/.zshenv"; fi
}

# Every file a block may have been written to, for --uninstall.
profile_candidates() {
  echo "$HOME/.profile"
  echo "$HOME/.bashrc"
  echo "$HOME/.bash_profile"
  echo "$HOME/.bash_login"
  echo "$HOME/.zshenv"
  if [[ -n "${ZDOTDIR:-}" && "$ZDOTDIR" != "$HOME" ]]; then echo "$ZDOTDIR/.zshenv"; fi
}

# The file holds exactly one block, equal to ours.
has_exact_block() {
  B="$PROFILE_BEGIN" L="$PROFILE_LINE" E="$PROFILE_END" awk '
    $0 == ENVIRON["B"] { n++; s = 1; next }
    s == 1 { s = ($0 == ENVIRON["L"]) ? 2 : 0; next }
    s == 2 { if ($0 == ENVIRON["E"]) ok++; s = 0; next }
    END { exit !(n == 1 && ok == 1) }
  ' "$1"
}

# Begin and end markers pair up (a hand-edited file may break that).
blocks_balanced() {
  B="$PROFILE_BEGIN" E="$PROFILE_END" awk '
    $0 == ENVIRON["B"] { if (open) bad = 1; open = 1; next }
    $0 == ENVIRON["E"] { if (!open) bad = 1; open = 0; next }
    END { exit (bad || open) }
  ' "$1"
}

has_block() {
  grep -qxF "$PROFILE_BEGIN" "$1" 2>/dev/null
}

# The file without any of our blocks, on stdout.
strip_blocks() {
  B="$PROFILE_BEGIN" E="$PROFILE_END" awk '
    $0 == ENVIRON["B"] { skip = 1; next }
    skip && $0 == ENVIRON["E"] { skip = 0; next }
    !skip { print }
  ' "$1"
}

# Rewrite a file in place through cat, so a symlinked dotfile stays a symlink
# and keeps its mode.
replace_contents() {
  cat "$2" >"$1"
}

# add_profile_block <file>: 0 when the file holds our block afterwards.
add_profile_block() {
  local f="$1" tmp
  if [[ -f "$f" ]] && has_exact_block "$f"; then
    return 0
  fi
  if [[ -f "$f" ]] && has_block "$f" && ! blocks_balanced "$f"; then
    PATH_WARNINGS+=("$(tilde "$f") has an unmatched toolu-runner marker; left unchanged")
    return 1
  fi
  mkdir -p "$(dirname "$f")"
  tmp="$WORK_DIR/profile.tmp"
  : >"$tmp"
  if [[ -f "$f" ]]; then
    strip_blocks "$f" >"$tmp"
  fi
  # A file without a final newline gets one, so the block starts on its own line.
  if [[ -s "$tmp" && -n "$(tail -c 1 "$tmp")" ]]; then
    echo >>"$tmp"
  fi
  printf '%s\n%s\n%s\n' "$PROFILE_BEGIN" "$PROFILE_LINE" "$PROFILE_END" >>"$tmp"
  replace_contents "$f" "$tmp"
}

# write_env_files <path guard 0|1>: ~/.toolu-runner/env and env.fish. They hold
# the install dir and TOOLU_RUNNER_HOME, never a secret.
write_env_files() {
  local with_path="$1"
  mkdir -p "$ENV_DIR"
  {
    echo "# toolu-runner shell environment, written by install.sh (undo: install.sh --uninstall)."
    echo "# No secrets belong here: tokens live in the runner's 0600 credential store."
    if [[ "$with_path" -eq 1 ]]; then
      printf 'case ":${PATH}:" in\n  *:"%s":*) ;;\n  *) export PATH="%s:$PATH" ;;\nesac\n' \
        "$INSTALL_DIR" "$INSTALL_DIR"
    fi
    if [[ -n "$RUNNER_HOME" ]]; then
      printf 'export TOOLU_RUNNER_HOME="%s"\n' "$RUNNER_HOME"
    fi
  } >"$ENV_DIR/env.tmp"
  mv -f "$ENV_DIR/env.tmp" "$ENV_DIR/env"
  {
    echo "# toolu-runner shell environment, written by install.sh (undo: install.sh --uninstall)."
    echo "# No secrets belong here: tokens live in the runner's 0600 credential store."
    if [[ "$with_path" -eq 1 ]]; then
      printf 'if not contains -- "%s" $PATH\n    set -gx PATH "%s" $PATH\nend\n' \
        "$INSTALL_DIR" "$INSTALL_DIR"
    fi
    if [[ -n "$RUNNER_HOME" ]]; then
      printf 'set -gx TOOLU_RUNNER_HOME "%s"\n' "$RUNNER_HOME"
    fi
  } >"$ENV_DIR/env.fish.tmp"
  mv -f "$ENV_DIR/env.fish.tmp" "$ENV_DIR/env.fish"
  chmod 0644 "$ENV_DIR/env" "$ENV_DIR/env.fish"
}

# --- --uninstall -----------------------------------------------------------------

uninstall() {
  [[ -n "${HOME:-}" ]] || die "--uninstall needs HOME to find the shell profiles"
  WORK_DIR="$(mktemp -d -t toolu-runner-install.XXXXXX)"
  trap 'rm -rf "$WORK_DIR"' EXIT
  local removed=() kept=() f
  while IFS= read -r f; do
    if [[ ! -f "$f" ]] || ! has_block "$f"; then continue; fi
    if ! blocks_balanced "$f"; then
      kept+=("$(tilde "$f")")
      continue
    fi
    strip_blocks "$f" >"$WORK_DIR/profile.tmp"
    replace_contents "$f" "$WORK_DIR/profile.tmp"
    removed+=("$(tilde "$f")")
  done < <(profile_candidates)
  for f in "$ENV_DIR/env" "$ENV_DIR/env.fish" "$FISH_CONF"; do
    if [[ -e "$f" ]]; then
      rm -f "$f"
      removed+=("$(tilde "$f")")
    fi
  done
  # Only when empty: the dir is also the default runner home.
  rmdir "$ENV_DIR" 2>/dev/null || true

  if [[ ${#removed[@]} -eq 0 ]]; then
    echo "$BIN_NAME: nothing to remove"
  elif [[ "$PRETTY" -eq 1 ]]; then
    row "✓" "$C_MUTED" "Removed" "$(join_and "${removed[@]}")"
  else
    echo "$BIN_NAME: removed $(join_and "${removed[@]}")"
  fi
  if [[ ${#kept[@]} -gt 0 ]]; then
    err "left unchanged (unmatched toolu-runner marker; edit by hand): $(join_and "${kept[@]}")"
  fi
  echo "$BIN_NAME: the binary and runner state were left in place"
  exit 0
}

if [[ "$UNINSTALL" -eq 1 ]]; then
  uninstall
fi

# --- semver validation -----------------------------------------------------

# Loose semver: MAJOR.MINOR.PATCH with optional pre-release / build metadata.
# Requires a leading 'v' to match our tag convention (v0.1.0).
SEMVER_RE='^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?(\+[0-9A-Za-z.-]+)?$'

validate_version() {
  local v="$1"
  if [[ ! "$v" =~ $SEMVER_RE ]]; then
    die "invalid --version '$v' (expected semver like v0.1.0, optionally with -prerelease or +build)"
  fi
}

if [[ -n "$VERSION" ]]; then
  validate_version "$VERSION"
fi

# --- arch + os detection ---------------------------------------------------

detect_arch() {
  local raw
  raw="$(uname -m)"
  case "$raw" in
    x86_64)            echo "amd64" ;;
    amd64)             echo "amd64" ;;
    aarch64|arm64)     echo "arm64" ;;
    *)
      die "unsupported architecture: $raw (supported: x86_64, aarch64/arm64)"
      ;;
  esac
}

detect_os() {
  local raw
  raw="$(uname -s)"
  case "$raw" in
    Darwin) echo "darwin" ;;
    Linux)  echo "linux"  ;;
    *)
      die "unsupported OS: $raw (supported: Darwin, Linux)"
      ;;
  esac
}

ARCH="$(detect_arch)"
OS="$(detect_os)"
ASSET="$BIN_NAME-$OS-$ARCH.tar.gz"

# --- URL construction ------------------------------------------------------

# A specific version is .../download/<tag>/<file>; "latest" is
# .../latest/download/<file>, which GitHub redirects to the latest release —
# no API call, so no jq. The tarball and its SHA256SUMS come from the same
# release directory.
release_url() {
  local file="$1"
  if [[ -n "$VERSION" ]]; then
    echo "$RELEASE_BASE/download/$VERSION/$file"
  else
    echo "$RELEASE_BASE/latest/download/$file"
  fi
}

VERSION_LABEL="${VERSION:-latest}"
ASSET_URL="$(release_url "$ASSET")"
SUMS_URL="$(release_url SHA256SUMS)"

# --- install dir resolution ------------------------------------------------

# A dir is usable when it exists and is writable, or does not exist and its
# nearest existing ancestor is writable (so mkdir -p will succeed).
dir_writable() {
  local d="$1"
  while [[ ! -e "$d" ]]; do
    local parent
    parent="$(dirname "$d")"
    [[ "$parent" != "$d" ]] || return 1
    d="$parent"
  done
  [[ -d "$d" && -w "$d" ]]
}

# Explicit --install-dir is used as given (an unwritable one fails at install
# time, no fallback). Otherwise prefer the default and fall back to
# ~/.local/bin when the default is not writable — the common case on Apple
# Silicon macOS (root-owned /usr/local/bin) and on any non-root Linux user.
if [[ -n "${HOME:-}" ]]; then
  FALLBACK_INSTALL_DIR="$HOME/.local/bin"
fi
if [[ "$INSTALL_DIR_EXPLICIT" -eq 1 ]]; then
  :
elif dir_writable "$DEFAULT_INSTALL_DIR" || [[ -z "$FALLBACK_INSTALL_DIR" ]]; then
  INSTALL_DIR="$DEFAULT_INSTALL_DIR"
else
  INSTALL_DIR="$FALLBACK_INSTALL_DIR"
  INSTALL_DIR_FALLBACK=1
fi
BIN="$INSTALL_DIR/$BIN_NAME"

# The static units in the tarball hardcode $STATIC_UNIT_BIN; a unit pointing at
# a binary that is not there would just crash-loop under launchd / systemd.
SERVICE_STATUS="no"
if [[ "$SERVICE" -eq 1 ]]; then
  if [[ "$BIN" == "$STATIC_UNIT_BIN" ]]; then
    SERVICE_STATUS="yes"
  else
    SERVICE_STATUS="skipped (static unit expects $STATIC_UNIT_BIN; run \"$BIN_NAME install-service\" after registering)"
  fi
fi

ON_PATH=0
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ON_PATH=1 ;;
esac

if [[ "$NO_MODIFY_PATH" -eq 1 ]]; then
  MODIFY_PATH_PLAN="no (--no-modify-path)"
elif [[ -z "$ENV_DIR" ]]; then
  MODIFY_PATH_PLAN="no (HOME is not set)"
elif [[ "$ON_PATH" -eq 1 && -z "$RUNNER_HOME" ]]; then
  MODIFY_PATH_PLAN="no ($INSTALL_DIR is already on PATH)"
else
  MODIFY_PATH_PLAN="yes ($ENV_DIR/env, sourced from the shell profiles)"
fi

# --- --check: print plan, exit --------------------------------------------

if [[ "$CHECK" -eq 1 ]]; then
  echo "install plan:"
  echo "  detected arch: $ARCH"
  echo "  detected os:   $OS"
  echo "  version:       $VERSION_LABEL"
  echo "  install dir:   $INSTALL_DIR"
  if [[ "$INSTALL_DIR_FALLBACK" -eq 1 ]]; then
    echo "  note:          $DEFAULT_INSTALL_DIR is not writable; using $INSTALL_DIR (pass --install-dir to override, or re-run with sudo)"
  fi
  echo "  service:       $SERVICE_STATUS"
  echo "  modify path:   $MODIFY_PATH_PLAN"
  if [[ "$NO_SETUP" -eq 1 ]]; then
    echo "  setup:         skipped (--no-setup)"
  else
    echo "  setup:         run \"$BIN_NAME setup\" when the release has it"
  fi
  if [[ ${#SETUP_ARGS[@]} -gt 0 ]]; then
    echo "  setup args:    $(join_args "${SETUP_ARGS[@]}")"
  else
    echo "  setup args:    (none)"
  fi
  echo "  would download: $ASSET_URL"
  echo "  would verify:  $SUMS_URL"
  echo "  would install to: $BIN"
  exit 0
fi

# --- tools (before anything is downloaded or written) ----------------------

for tool in curl tar; do
  command -v "$tool" >/dev/null 2>&1 || { err "$tool is required"; exit 1; }
done
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{ print $1 }'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | awk '{ print $1 }'; }
else
  err "sha256sum or shasum is required to verify the download; nothing was installed"
  exit 1
fi

# --- prepare install dir (before downloading anything) ---------------------

if [[ ! -d "$INSTALL_DIR" ]]; then
  if mkdir -p "$INSTALL_DIR" 2>/dev/null; then
    :
  else
    err "install dir $INSTALL_DIR does not exist and could not be created"
    err "re-run with sudo, or pass --install-dir <writable dir>"
    exit 1
  fi
fi

if [[ ! -w "$INSTALL_DIR" ]]; then
  err "no write permission to $INSTALL_DIR"
  err "re-run with sudo, or pass --install-dir <writable dir>"
  exit 1
fi

# --- download + verify -----------------------------------------------------

# Not $TMPDIR: that name is the environment's temp dir, and setup inherits it.
WORK_DIR="$(mktemp -d -t toolu-runner-install.XXXXXX)"
NEW_BIN="$INSTALL_DIR/.$BIN_NAME.new.$$"
cleanup() { rm -rf "$WORK_DIR"; rm -f "$NEW_BIN"; }
trap cleanup EXIT

ARCHIVE="$WORK_DIR/$ASSET"
SUMS="$WORK_DIR/SHA256SUMS"

if [[ "$PRETTY" -eq 1 ]]; then
  printf '\n  %s installer\n\n' "$BIN_NAME"
fi

download() {
  local url="$1" out="$2"
  if ! curl -fsSL --retry 3 -o "$out" "$url"; then
    cat >&2 <<EOF

download failed: $url

This usually means no release artifact exists for $OS/$ARCH at tag $VERSION_LABEL yet.
Check that:
  - a release tagged $VERSION_LABEL exists at $RELEASE_BASE
  - the release has assets named $ASSET and SHA256SUMS

No files were installed to $INSTALL_DIR.
EOF
    exit 1
  fi
}

download "$ASSET_URL" "$ARCHIVE"
download "$SUMS_URL" "$SUMS"

# The release workflow writes "<hash>  <asset>"; "*<asset>" is the binary-mode
# spelling of the same line.
EXPECTED="$(A="$ASSET" awk '$2 == ENVIRON["A"] || $2 == "*" ENVIRON["A"] { print $1; exit }' "$SUMS" \
  | tr 'A-F' 'a-f')"
if [[ ! "$EXPECTED" =~ ^[0-9a-f]{64}$ ]]; then
  fail_install "SHA256SUMS has no entry for $ASSET."
fi
ACTUAL="$(sha256_of "$ARCHIVE")"
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
  fail_install "SHA256 mismatch for $ASSET." "Re-run, or report it if it repeats."
fi
SIZE_BYTES="$(wc -c <"$ARCHIVE" | tr -d ' ')"

# --- extract ---------------------------------------------------------------

# Most release tarballs put the binary at the root; some nest it under a single
# directory. Handle both.
EXTRACT_DIR="$WORK_DIR/extract"
mkdir -p "$EXTRACT_DIR"
tar -xzf "$ARCHIVE" -C "$EXTRACT_DIR"

BIN_PATH=""
if [[ -x "$EXTRACT_DIR/$BIN_NAME" ]]; then
  BIN_PATH="$EXTRACT_DIR/$BIN_NAME"
elif [[ -x "$EXTRACT_DIR/$BIN_NAME/$BIN_NAME" ]]; then
  BIN_PATH="$EXTRACT_DIR/$BIN_NAME/$BIN_NAME"
else
  err "could not find $BIN_NAME binary in extracted archive:"
  err "$(ls -la "$EXTRACT_DIR" 2>/dev/null || true)"
  exit 1
fi

# --- install binary --------------------------------------------------------

# Stage beside the target, ask the staged binary its version (from the install
# dir, so a noexec /tmp does not matter), then rename over the old one: a
# binary that does not run never replaces one that does, and a running service
# keeps its old inode.
install -m 0755 "$BIN_PATH" "$NEW_BIN"
INSTALLED_VERSION="$("$NEW_BIN" --version 2>/dev/null </dev/null \
  | awk -v n="$BIN_NAME" 'NR == 1 && $1 == n { print $2 }')" || INSTALLED_VERSION=""
if [[ -z "$INSTALLED_VERSION" ]]; then
  fail_install "The downloaded $BIN_NAME does not run on this machine ($OS/$ARCH)."
fi
mv -f "$NEW_BIN" "$BIN"

# --- optional service install ---------------------------------------------

install_service() {
  local scripts_dir="$EXTRACT_DIR/scripts"
  if [[ ! -d "$scripts_dir" ]]; then
    # Fall back: scripts dir may be nested under the versioned top-level dir.
    scripts_dir="$EXTRACT_DIR/$BIN_NAME/scripts"
  fi

  case "$OS" in
    darwin)
      local plist_src="$scripts_dir/io.$BIN_NAME.plist"
      local plist_dst="$HOME/Library/LaunchAgents/io.$BIN_NAME.plist"
      if [[ ! -f "$plist_src" ]]; then
        err "--service requested but $plist_src was not found in the release tarball"
        err "the tarball may be too old or corrupt; re-download, or install without --service"
        return 1
      fi
      mkdir -p "$(dirname "$plist_dst")"
      cp "$plist_src" "$plist_dst"
      chmod 0644 "$plist_dst"
      if command -v launchctl >/dev/null 2>&1; then
        launchctl unload "$plist_dst" 2>/dev/null || true
        launchctl load "$plist_dst"
        echo "loaded launchd agent: $plist_dst"
      else
        err "launchctl not found; copied plist to $plist_dst but did not load it"
      fi
      ;;
    linux)
      local unit_src="$scripts_dir/$BIN_NAME.service"
      local unit_dst="/etc/systemd/system/$BIN_NAME.service"
      if [[ ! -f "$unit_src" ]]; then
        err "--service requested but $unit_src was not found in the release tarball"
        err "the tarball may be too old or corrupt; re-download, or install without --service"
        return 1
      fi
      if [[ $EUID -ne 0 ]]; then
        err "--service on Linux requires root; re-run with sudo"
        return 1
      fi
      cp "$unit_src" "$unit_dst"
      chmod 0644 "$unit_dst"
      if command -v systemctl >/dev/null 2>&1; then
        systemctl daemon-reload
        systemctl enable --now "$BIN_NAME.service"
        echo "enabled + started systemd unit: $unit_dst"
      else
        err "systemctl not found; copied unit to $unit_dst but did not start it"
      fi
      ;;
  esac
}

if [[ "$SERVICE" -eq 1 ]]; then
  if [[ "$SERVICE_STATUS" != "yes" ]]; then
    err "--service skipped: the static unit in the tarball runs $STATIC_UNIT_BIN, but the binary is at $BIN"
    err "after registering, run \"$BIN install-service\" — it writes a unit for this binary's real path"
  elif ! install_service; then
    err "binary was installed but service setup failed; fix the issue and re-run with --service"
    exit 1
  fi
fi

# --- environment for new shells ----------------------------------------------

# PATH_ROW / PATH_GLYPH describe the outcome; ENV_WRITTEN says whether new
# shells will find the binary by name.
PATH_WARNINGS=()
PATH_GLYPH="✓"
PATH_ROW=""
ENV_WRITTEN=0
MANUAL_LINES=()
if [[ "$NO_MODIFY_PATH" -eq 1 || -z "$ENV_DIR" ]]; then
  if [[ "$ON_PATH" -eq 1 ]]; then
    PATH_ROW="$(tilde "$INSTALL_DIR") is already on PATH"
  else
    PATH_GLYPH="▲"
    if [[ -z "$ENV_DIR" ]]; then
      PATH_ROW="not changed (HOME is not set); add it yourself:"
    else
      PATH_ROW="not changed (--no-modify-path); add it yourself:"
    fi
    MANUAL_LINES+=("export PATH=\"$INSTALL_DIR:\$PATH\"")
  fi
  if [[ -n "$RUNNER_HOME" ]]; then
    MANUAL_LINES+=("export TOOLU_RUNNER_HOME=\"$RUNNER_HOME\"")
  fi
elif [[ "$ON_PATH" -eq 1 && -z "$RUNNER_HOME" ]]; then
  PATH_ROW="$(tilde "$INSTALL_DIR") is already on PATH"
elif [[ "$ON_PATH" -eq 0 ]] && unsafe_for_shell_file "$INSTALL_DIR"; then
  PATH_GLYPH="▲"
  PATH_ROW="not changed (the install dir cannot be written to a shell file safely); add it yourself:"
  MANUAL_LINES+=("export PATH=\"$INSTALL_DIR:\$PATH\"")
else
  WITH_PATH=1
  if [[ "$ON_PATH" -eq 1 ]]; then WITH_PATH=0; fi
  write_env_files "$WITH_PATH"
  CONFIGURED=()
  while IFS= read -r f; do
    if add_profile_block "$f"; then CONFIGURED+=("$(tilde "$f")"); fi
  done < <(profile_targets)
  if command -v fish >/dev/null 2>&1 || [[ -d "$(dirname "$(dirname "$FISH_CONF")")" ]]; then
    mkdir -p "$(dirname "$FISH_CONF")"
    {
      echo "# toolu-runner (written by install.sh; undo: install.sh --uninstall)"
      echo 'if test -f "$HOME/.toolu-runner/env.fish"; source "$HOME/.toolu-runner/env.fish"; end'
    } >"$FISH_CONF"
    CONFIGURED+=("fish")
  fi
  ENV_WRITTEN=1
  if [[ ${#CONFIGURED[@]} -eq 0 ]]; then
    PATH_GLYPH="▲"
    PATH_ROW="no shell profile could be changed; run: . $(tilde "$ENV_DIR/env")"
  elif [[ "$WITH_PATH" -eq 1 ]]; then
    PATH_ROW="added to $(join_and "${CONFIGURED[@]}")"
  else
    PATH_ROW="TOOLU_RUNNER_HOME added to $(join_and "${CONFIGURED[@]}")"
  fi
fi

# --- report ------------------------------------------------------------------

if [[ "$PRETTY" -eq 1 ]]; then
  size="$(awk -v b="$SIZE_BYTES" 'BEGIN { printf "%.1f MB", b / 1048576 }')"
  row "✓" "$C_MUTED" "Downloaded" "$BIN_NAME $INSTALLED_VERSION · $OS-$ARCH · $size"
  row "✓" "$C_MUTED" "Verified" "SHA256SUMS match"
  row "✓" "$C_MUTED" "Installed" "$(tilde "$BIN")"
  if [[ "$PATH_GLYPH" == "✓" ]]; then
    row "✓" "$C_MUTED" "PATH" "$PATH_ROW"
  else
    row "▲" "$C_WARN" "PATH" "$PATH_ROW"
  fi
  for line in ${MANUAL_LINES[@]+"${MANUAL_LINES[@]}"}; do
    printf '                 %s\n' "$line"
  done
else
  echo "$BIN_NAME: installed $INSTALLED_VERSION (checksum ok)"
  if [[ ${#MANUAL_LINES[@]} -gt 0 ]]; then
    echo "$BIN_NAME: PATH not changed; add: $(join_args "${MANUAL_LINES[@]}")"
  fi
fi
for w in ${PATH_WARNINGS[@]+"${PATH_WARNINGS[@]}"}; do
  err "$w"
done

# --- hand off to setup -------------------------------------------------------

HAS_SETUP=0
if "$BIN" setup --help >/dev/null 2>&1 </dev/null; then HAS_SETUP=1; fi
TTY_OK=0
if (exec </dev/tty) 2>/dev/null; then TTY_OK=1; fi

# The command to print: by name when new shells (or this one) will find it.
next_line() {
  local cmd="$BIN" hint=""
  if [[ "$ON_PATH" -eq 1 ]]; then
    cmd="$BIN_NAME"
  elif [[ "$ENV_WRITTEN" -eq 1 && "$PATH_GLYPH" == "✓" ]]; then
    cmd="$BIN_NAME"
    hint="  (in a new shell, or after: . $(tilde "$ENV_DIR/env"))"
  fi
  if [[ "$HAS_SETUP" -eq 1 ]]; then
    cmd="$cmd setup"
  else
    cmd="$cmd register --url https://github.com/<owner>/<repo>"
  fi
  if [[ "$PRETTY" -eq 1 ]]; then
    printf '  %s→%s next: %s%s\n' "$C_ACCENT" "$C_RESET" "$cmd" "$hint"
  else
    echo "next: $cmd$hint"
  fi
}

run_setup() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) export PATH="$INSTALL_DIR:$PATH" ;;
  esac
  if [[ -n "$RUNNER_HOME" ]]; then export TOOLU_RUNNER_HOME="$RUNNER_HOME"; fi
  # exec skips the EXIT trap.
  cleanup
  trap - EXIT
  if [[ "$1" == "tty" ]]; then
    if [[ "$PRETTY" -eq 1 ]]; then
      printf '  %s→%s Starting setup…\n\n' "$C_ACCENT" "$C_RESET"
    fi
    exec "$BIN" setup ${SETUP_ARGS[@]+"${SETUP_ARGS[@]}"} </dev/tty
  fi
  # Under curl | bash stdin is this script: setup never reads it.
  exec "$BIN" setup ${SETUP_ARGS[@]+"${SETUP_ARGS[@]}"} </dev/null
}

if [[ "$NO_SETUP" -eq 1 ]]; then
  next_line
elif [[ "$HAS_SETUP" -eq 1 && "$TTY_OK" -eq 1 ]]; then
  run_setup tty
elif [[ "$HAS_SETUP" -eq 1 && ${#SETUP_ARGS[@]} -gt 0 ]]; then
  run_setup null
elif [[ ${#SETUP_ARGS[@]} -gt 0 ]]; then
  err "$BIN_NAME $INSTALLED_VERSION has no setup command; these options were not used: $(join_args "${SETUP_ARGS[@]}")"
  err "the binary is installed at $BIN; run \"$BIN --help\" to see its commands"
  exit 2
else
  next_line
fi
