#!/usr/bin/env bash
# install.sh — install toolu-runner from GitHub releases.
#
# Mirrors actions/runner's install.sh UX:
#   - detect arch (x86_64 / aarch64 / arm64)
#   - detect OS (darwin / linux)
#   - download the matching release artifact
#   - install to /usr/local/bin/toolu-runner (or --install-dir)
#   - optionally install + start the service unit (--service)
#
# Usage:
#   bash install.sh                           # install latest release
#   bash install.sh --version v0.1.0          # install a specific release
#   bash install.sh --install-dir /opt/bin    # custom install dir
#   bash install.sh --service                 # also install launchd / systemd unit
#   bash install.sh --check                   # print the install plan, no download
#   bash install.sh --help                    # show this message
#
# Requires: bash 4+, curl, tar, install (or cp + chmod).
set -euo pipefail

REPO="${TOOLU_RUNNER_REPO:-Falconiere/toolu-ghrunner}"
BIN_NAME="toolu-runner"

INSTALL_DIR="/usr/local/bin"
VERSION=""
SERVICE=0
CHECK=0

usage() {
  cat <<EOF
install.sh — install $BIN_NAME from GitHub releases

USAGE:
  install.sh [OPTIONS]

OPTIONS:
  --version <v>        Install a specific version (e.g. v0.1.0). Default: latest.
  --install-dir <dir>  Directory to install the binary into. Default: /usr/local/bin.
  --service            Also install + start the service unit (launchd plist on macOS,
                       systemd unit on Linux). Requires the matching service file in
                       the release tarball at scripts/.
  --check              Print the install plan and exit. No download, no install.
  --help, -h           Show this message and exit.

ENVIRONMENT:
  TOOLU_RUNNER_REPO    Override the GitHub owner/repo. Default: $REPO.

EXAMPLES:
  # Print the plan without downloading.
  bash install.sh --check

  # Install the latest release.
  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | bash

  # Install v0.1.0 to /opt/bin and set up the service.
  bash install.sh --version v0.1.0 --install-dir /opt/bin --service

After install, register the runner with:
  $BIN_NAME register --url <repo_url> --token <reg_token> --name <name> --labels self-hosted,<os>,<arch>
EOF
}

err() {
  echo "install.sh: $*" >&2
}

die() {
  err "$@"
  exit 2
}

# --- arg parsing -----------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      [[ $# -ge 2 ]] || die "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --install-dir)
      [[ $# -ge 2 ]] || die "--install-dir requires a value"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --service)
      SERVICE=1
      shift
      ;;
    --check)
      CHECK=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    -*)
      die "unknown option: $1 (try --help)"
      ;;
    *)
      die "unexpected positional arg: $1 (try --help)"
      ;;
  esac
done

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

# --- URL construction ------------------------------------------------------

# For a specific version we use .../download/<tag>/<asset>.
# For "latest" we use .../latest/download/<asset> so GitHub resolves it.
asset_url() {
  local version="$1" os="$2" arch="$3"
  local asset="$BIN_NAME-$os-$arch.tar.gz"
  if [[ -n "$version" ]]; then
    echo "https://github.com/$REPO/releases/download/$version/$asset"
  else
    echo "https://github.com/$REPO/releases/latest/download/$asset"
  fi
}

VERSION_LABEL="${VERSION:-latest}"
ASSET_URL="$(asset_url "$VERSION" "$OS" "$ARCH")"

# --- --check: print plan, exit --------------------------------------------

if [[ "$CHECK" -eq 1 ]]; then
  echo "install plan:"
  echo "  detected arch: $ARCH"
  echo "  detected os:   $OS"
  echo "  version:       $VERSION_LABEL"
  echo "  install dir:   $INSTALL_DIR"
  echo "  service:       $([[ $SERVICE -eq 1 ]] && echo yes || echo no)"
  echo "  would download: $ASSET_URL"
  echo "  would install to: $INSTALL_DIR/$BIN_NAME"
  exit 0
fi

# --- resolve latest version (only when actually installing) ----------------

if [[ -z "$VERSION" ]]; then
  if ! command -v curl >/dev/null 2>&1; then
    die "curl is required to resolve the latest version"
  fi
  if ! command -v jq >/dev/null 2>&1; then
    die "jq is required to resolve the latest version (or pass --version explicitly)"
  fi
  VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | jq -r '.tag_name // empty')"
  if [[ -z "$VERSION" ]]; then
    die "could not determine latest release from https://api.github.com/repos/$REPO/releases/latest"
  fi
  validate_version "$VERSION"
  VERSION_LABEL="$VERSION"
  ASSET_URL="$(asset_url "$VERSION" "$OS" "$ARCH")"
fi

# --- download + extract ----------------------------------------------------

TMPDIR="$(mktemp -d -t toolu-runner-install.XXXXXX)"
cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT

ARCHIVE="$TMPDIR/$BIN_NAME.tar.gz"

echo "installing $BIN_NAME $VERSION_LABEL"
echo "  arch:        $ARCH"
echo "  os:          $OS"
echo "  install dir: $INSTALL_DIR"
echo "  download:    $ASSET_URL"

if ! curl -fsSL --retry 3 -o "$ARCHIVE" "$ASSET_URL"; then
  cat >&2 <<EOF

download failed: $ASSET_URL

This usually means no release artifact exists for $OS/$ARCH at tag $VERSION_LABEL yet.
Check that:
  - a release tagged $VERSION_LABEL exists at https://github.com/$REPO/releases
  - the release has an asset named $(basename "$ASSET_URL")

No files were installed to $INSTALL_DIR.
EOF
  exit 1
fi

# Extract. Most release tarballs put the binary at the root; some nest it
# under a single directory. Handle both.
EXTRACT_DIR="$TMPDIR/extract"
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

install -m 0755 "$BIN_PATH" "$INSTALL_DIR/$BIN_NAME"
echo "installed $BIN_NAME $VERSION_LABEL to $INSTALL_DIR/$BIN_NAME"

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
  if ! install_service; then
    err "binary was installed but service setup failed; fix the issue and re-run with --service"
    exit 1
  fi
fi

# --- next-steps ------------------------------------------------------------

cat <<EOF

$BIN_NAME $VERSION_LABEL installed to $INSTALL_DIR/$BIN_NAME

Next: register the runner with your repository or org:

  $INSTALL_DIR/$BIN_NAME register \\
    --url <repo_url_or_org_url> \\
    --token <registration_token> \\
    --name <runner_name> \\
    --labels self-hosted,$OS,$ARCH

The registration token comes from:
  repo:    Settings -> Actions -> Runners -> "New self-hosted runner"
  org:     Settings -> Actions -> Runners -> "New self-hosted runner"

Run "$INSTALL_DIR/$BIN_NAME --help" to see all commands.
EOF
