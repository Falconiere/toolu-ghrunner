#!/usr/bin/env bash
# Run issue #72's production job-container replay inside Linux with real Docker.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
linux_root=${TOOLU_CI72_LINUX_ROOT:-"$HOME/.cache/toolu-ghrunner-ci72-linux"}
mkdir -p "$linux_root" "$linux_root/tmp"
rsync -a --delete --exclude=.git --exclude=target "$repo_root/" "$linux_root/source/"

docker run --rm \
  --mount "type=bind,src=$linux_root,dst=$linux_root" \
  --mount type=volume,src=toolu_ci72_cargo_target,dst=/cargo-target \
  --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock \
  --workdir "$linux_root/source" \
  --env CARGO_TARGET_DIR=/cargo-target \
  --env DOCKER_HOST=unix:///var/run/docker.sock \
  --env TOOLU_CONTAINER_TEST_ROOT="$linux_root/tmp" \
  rust:1.94.1 \
  bash -c '
    set -euo pipefail
    test "$(uname -s)" = Linux
    test -S /var/run/docker.sock
    listed=$(cargo test -p execution --test job_container_ci_test -- --list)
    case "$listed" in
      *container_defaults_to_runner_ci_or_true*container_preserves_declaration_and_step_overrides_across_handlers*) ;;
      *) echo "required Linux container tests are missing" >&2; exit 2 ;;
    esac
    mkdir -p "$TOOLU_CONTAINER_TEST_ROOT"
    env -u CI cargo test -p execution --test job_container_ci_test -- --ignored --nocapture
    CI=false cargo test -p execution --test job_container_ci_test -- --ignored --nocapture
    CI=runner cargo test -p execution --test job_container_ci_test -- --ignored --nocapture
  '
