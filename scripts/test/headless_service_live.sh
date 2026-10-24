#!/usr/bin/env bash
# Dedicated VPS lane for #203. The target must be disposable, have a non-root
# SSH login, a registered toolu runner, passwordless sudo reboot, and a
# workflow_dispatch workflow with inputs nonce/expected_path. Its job must run
# on that runner, assert PATH == expected_path, and print HEADLESS_PATH=$PATH.
set -euo pipefail

for name in TOOLU_HEADLESS_SSH TOOLU_HEADLESS_REPO TOOLU_HEADLESS_WORKFLOW \
  TOOLU_HEADLESS_EXPECTED_PATH; do
  if [[ -z "${!name:-}" ]]; then
    echo "UNVERIFIED: $name is required for the dedicated VPS lane" >&2
    exit 2
  fi
done
if [[ "${TOOLU_HEADLESS_ALLOW_REBOOT:-}" != 1 ]]; then
  echo 'UNVERIFIED: set TOOLU_HEADLESS_ALLOW_REBOOT=1 for a disposable VPS' >&2
  exit 2
fi
command -v gh >/dev/null || { echo 'gh is required' >&2; exit 2; }
command -v jq >/dev/null || { echo 'jq is required' >&2; exit 2; }

remote() { ssh -o BatchMode=yes -o ConnectTimeout=8 "$TOOLU_HEADLESS_SSH" "$@"; }
check_service() {
  remote 'test "$(id -u)" -ne 0 && \
    test "$(loginctl show-user "$(id -un)" --property=Linger)" = Linger=yes && \
    systemctl --user is-active --quiet toolu-runner.service'
}

if ! remote 'test "$(id -u)" -ne 0 && toolu-runner install-service --enable-linger'; then
  echo 'FAIL: non-root install or linger enable failed' >&2
  exit 1
fi
check_service
# The first SSH session has ended; a new login proves the user manager stayed
# active after logout.
check_service
unit_env=$(remote 'systemctl --user show -P Environment toolu-runner.service')
if [[ "$unit_env" != *"PATH=$TOOLU_HEADLESS_EXPECTED_PATH"* ]]; then
  echo 'FAIL: the installed unit does not declare TOOLU_HEADLESS_EXPECTED_PATH' >&2
  exit 1
fi

# SSH may drop while systemd starts the reboot. Require an observed outage and
# a later reconnect, so a refused sudo/reboot cannot be reported as passing.
remote 'sudo -n systemctl reboot' || true
down=0
for _ in {1..30}; do
  if ! remote true >/dev/null 2>&1; then down=1; break; fi
  sleep 2
done
if [[ "$down" != 1 ]]; then echo 'FAIL: VPS did not go down for reboot' >&2; exit 1; fi
up=0
for _ in {1..90}; do
  if remote true >/dev/null 2>&1; then up=1; break; fi
  sleep 5
done
if [[ "$up" != 1 ]]; then echo 'FAIL: VPS did not reconnect after reboot' >&2; exit 1; fi
check_service

nonce="headless-$(date -u +%Y%m%d%H%M%S)-$$"
gh workflow run "$TOOLU_HEADLESS_WORKFLOW" --repo "$TOOLU_HEADLESS_REPO" \
  -f "nonce=$nonce" -f "expected_path=$TOOLU_HEADLESS_EXPECTED_PATH"
run_id=''
for _ in {1..30}; do
  run_id=$(gh run list --repo "$TOOLU_HEADLESS_REPO" \
    --workflow "$TOOLU_HEADLESS_WORKFLOW" --limit 30 \
    --json databaseId,displayTitle | \
    jq -r --arg title "$nonce" '.[] | select(.displayTitle == $title) | .databaseId' | head -n 1)
  if [[ -n "$run_id" ]]; then break; fi
  sleep 2
done
if [[ -z "$run_id" ]]; then
  echo "FAIL: dispatched workflow run $nonce was not found" >&2
  exit 1
fi
timeout 1800 gh run watch "$run_id" --repo "$TOOLU_HEADLESS_REPO" --exit-status
gh run view "$run_id" --repo "$TOOLU_HEADLESS_REPO" --log | \
  grep -F "HEADLESS_PATH=$TOOLU_HEADLESS_EXPECTED_PATH" >/dev/null

# On the same disposable host, check the root path and real system manager
# after the non-root job has finished. Root has a separate runner home.
if ! remote 'sudo -n toolu-runner install-service && \
  sudo -n test -f /etc/systemd/system/toolu-runner.service && \
  sudo -n systemctl is-enabled --quiet toolu-runner.service && \
  test "$(sudo -n systemctl show -P FragmentPath toolu-runner.service)" = /etc/systemd/system/toolu-runner.service'; then
  echo 'FAIL: root system unit install or verification failed' >&2
  exit 1
fi
remote 'sudo -n toolu-runner install-service --remove'
echo "headless live check passed: https://github.com/$TOOLU_HEADLESS_REPO/actions/runs/$run_id"
