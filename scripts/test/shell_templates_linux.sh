#!/usr/bin/env bash
# Run issue #80's captured job-container shell-template replays on Linux.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
linux_root=${TOOLU_SHELL_TEMPLATES_LINUX_ROOT:-"$HOME/.cache/toolu-ghrunner-shell-templates80"}
mkdir -p "$linux_root/tmp"
rsync -a --delete --exclude=.git --exclude=target "$repo_root/" "$linux_root/source/"

docker run --rm \
  --mount "type=bind,src=$linux_root,dst=$linux_root" \
  --mount type=volume,src=toolu_shell_templates80_cargo_target,dst=/cargo-target \
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
    listed=$(cargo test -p execution --test shell_templates_container_test -- --list)
    for required in \
      container_default_shell_is_sh_and_pipeline_succeeds \
      container_explicit_bash_pipeline_fails \
      container_custom_template_preserves_quoted_argument_and_mounted_script_path \
      container_composite_custom_template_uses_mounted_script_path \
      container_unknown_shell_fails_before_running_body; do
      case "$listed" in
        *"$required: test"*) ;;
        *) echo "required Linux container test is missing: $required" >&2; exit 2 ;;
      esac
    done
    mkdir -p "$TOOLU_CONTAINER_TEST_ROOT"
    cargo test -p execution --test shell_templates_test --test shell_templates_replay_test
    cargo test -p execution --test shell_templates_container_test -- --ignored --nocapture
  '
