#!/usr/bin/env bash
# Run issue #75's Docker-action runtime and production replay in Linux.
set -euo pipefail

mode=${1:-all}
case "$mode" in
  runtime|replay|remote|gate|all) ;;
  *) printf 'usage: %s [runtime|replay|remote|gate|all]\n' "$0" >&2; exit 2 ;;
esac
if [[ "$mode" = remote ]]; then
  : "${TOOLU_DOCKER_ACTIONS_REMOTE_REF:?remote mode requires TOOLU_DOCKER_ACTIONS_REMOTE_REF}"
  : "${TOOLU_DOCKER_ACTIONS_REMOTE_TOKEN:?remote mode requires TOOLU_DOCKER_ACTIONS_REMOTE_TOKEN}"
fi
if [[ "$mode" = gate && -z ${TOOLU_DOCKER_ACTIONS_TEST_IMAGE:-} ]]; then
  echo "gate mode requires TOOLU_DOCKER_ACTIONS_TEST_IMAGE with jq and ast-grep" >&2
  exit 2
fi

repo_root=$(git rev-parse --show-toplevel)
linux_root=${TOOLU_DOCKER_ACTIONS_LINUX_ROOT:-"$HOME/.cache/toolu-ghrunner-docker-actions-linux"}
carrier_image=${TOOLU_DOCKER_ACTIONS_TEST_IMAGE:-rust:1.94.1}
mkdir -p "$linux_root" "$linux_root/tmp"
rsync -a --delete --exclude=.git --exclude=.codex --exclude=target "$repo_root/" "$linux_root/source/"

docker run --rm \
  --mount "type=bind,src=$linux_root,dst=$linux_root" \
  --mount type=volume,src=toolu_docker_actions_cargo_target,dst=/cargo-target \
  --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock \
  --workdir "$linux_root/source" \
  --env CARGO_TARGET_DIR=/cargo-target \
  --env CARGO_BUILD_JOBS="${TOOLU_DOCKER_ACTIONS_BUILD_JOBS:-2}" \
  --env CARGO_INCREMENTAL \
  --env RUST_TEST_THREADS \
  --env DOCKER_HOST=unix:///var/run/docker.sock \
  --env TOOLU_CONTAINER_TEST_ROOT="$linux_root/tmp" \
  --env TOOLU_DOCKER_ACTIONS_MODE="$mode" \
  --env TOOLU_DOCKER_ACTIONS_REMOTE_REF \
  --env TOOLU_DOCKER_ACTIONS_REMOTE_TOKEN \
  "$carrier_image" \
  bash -c '
    set -euo pipefail
    test "$(uname -s)" = Linux
    test -S /var/run/docker.sock
    mkdir -p "$TOOLU_CONTAINER_TEST_ROOT"

    if [[ "$TOOLU_DOCKER_ACTIONS_MODE" = runtime || "$TOOLU_DOCKER_ACTIONS_MODE" = all ]]; then
      listed=$(cargo test -p execution --lib docker::action_container -- --list)
      case "$listed" in
        *action_container*) ;;
        *) echo "required docker::action_container runtime tests are missing" >&2; exit 2 ;;
      esac
      cargo test -p execution --lib docker::action_container -- --ignored --nocapture --test-threads=1
    fi

    if [[ "$TOOLU_DOCKER_ACTIONS_MODE" = replay || "$TOOLU_DOCKER_ACTIONS_MODE" = all ]]; then
      cargo test -p execution --test docker_action_test -- --skip remote_repository_action
      listed=$(cargo test -p execution --test docker_action_linux_test -- --list)
      for required in \
        local_actions_preserve_argv_env_commands_state_and_lifo_posts \
        registry_and_manifest_arg_variants_execute_exact_argv \
        action_joins_job_network_and_reaches_real_peer \
        docker_action_joins_service_only_network_through_main_and_post \
        entrypoint_failure_and_cancellation_fail_visibly_without_leaking_containers
      do
        case "$listed" in
          *"$required"*) ;;
          *) echo "required Linux Docker-action test is missing: $required" >&2; exit 2 ;;
        esac
      done
      cargo test -p execution --test docker_action_linux_test -- \
        --ignored --nocapture --test-threads=1 --skip remote_repository_action
    fi

    if [[ "$TOOLU_DOCKER_ACTIONS_MODE" = remote ]]; then
      listed=$(cargo test -p execution --test docker_action_test -- --list)
      case "$listed" in
        *remote_repository_action_runs_pre_main_post_and_reuses_image*) ;;
        *) echo "required remote Docker-action replay is missing" >&2; exit 2 ;;
      esac
      cargo test -p execution --test docker_action_test \
        remote_repository_action -- --ignored --nocapture --test-threads=1
    fi

    if [[ "$TOOLU_DOCKER_ACTIONS_MODE" = gate ]]; then
      command -v jq >/dev/null
      command -v ast-grep >/dev/null
      git init -q
      git add .
      ./tools/check.sh all
    fi
  '
