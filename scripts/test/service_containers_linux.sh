#!/usr/bin/env bash
# Execute #74 production service replay on Linux against a real Docker daemon.
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
linux_root=${TOOLU_SERVICE_TEST_ROOT:-"$HOME/.cache/toolu-ghrunner-services74"}
mkdir -p "$linux_root/source" "$linux_root/tmp" "$linux_root/auth" "$linux_root/docker-config"
rsync -a --delete --exclude=.git --exclude=target "$repo_root/" "$linux_root/source/"
daemon_host=${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}')}
registry_id=''
registry_image=''
cleanup() {
  if [[ -n "$registry_id" ]]; then docker rm -fv "$registry_id"; fi
  if [[ -n "$registry_image" ]]; then docker image rm "$registry_image"; fi
}
trap cleanup EXIT
# Disposable local credentials; neither authenticates to any external service.
docker run --rm httpd:2-alpine htpasswd -Bbn service-user service-password > "$linux_root/auth/htpasswd"
registry_id=$(docker run -d -p 127.0.0.1::5000 \
  --mount "type=bind,src=$linux_root/auth,dst=/auth,readonly" \
  -e REGISTRY_AUTH=htpasswd -e REGISTRY_AUTH_HTPASSWD_REALM=service-test \
  -e REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd registry:2)
registry_port=$(docker port "$registry_id" 5000/tcp | awk -F: '{print $NF}')
registry_image="127.0.0.1:$registry_port/service74:fixture"
python3 - "$linux_root/docker-config/config.json" "127.0.0.1:$registry_port" <<'PYAUTH'
import base64, json, os, sys
with open(sys.argv[1], 'w') as output:
    json.dump({'auths': {sys.argv[2]: {'auth': base64.b64encode(b'service-user:service-password').decode()}}}, output)
os.chmod(sys.argv[1], 0o600)
PYAUTH
for attempt in {1..30}; do
  status=$(curl --silent --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:$registry_port/v2/") || status=000
  if [[ "$status" == 401 ]]; then break; fi
  if [[ "$attempt" == 30 ]]; then echo 'private registry did not become ready' >&2; exit 1; fi
  sleep 1
done
docker pull nginx:1.27-alpine
docker tag nginx:1.27-alpine "$registry_image"
docker --host "$daemon_host" --config "$linux_root/docker-config" push "$registry_image"
docker run --rm --network host \
  --mount "type=bind,src=$linux_root,dst=$linux_root" \
  --mount type=volume,src=toolu_services74_target,dst=/cargo-target \
  --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock \
  --workdir "$linux_root/source" \
  --env CARGO_TARGET_DIR=/cargo-target \
  --env DOCKER_HOST=unix:///var/run/docker.sock \
  --env TOOLU_CONTAINER_TEST_ROOT="$linux_root/tmp" \
  --env TOOLU_SERVICE_REGISTRY_IMAGE="$registry_image" \
  "${TOOLU_SERVICE_TEST_IMAGE:-rust:1.94.1}" bash -c '
    set -euo pipefail
    test "$(uname -s)" = Linux
    test -S /var/run/docker.sock
    listed=$(cargo test -p execution --test service_containers_test -- --list)
    case "$listed" in *services_host_dynamic_port_connects_and_cleans_up*) ;; *) exit 2 ;; esac
    cargo test -p execution --lib container_health_options -- --nocapture
    cargo test -p execution --test service_containers_test -- --include-ignored --nocapture
    cargo test -p execution --lib never_healthy_service_respects_budget -- --ignored --nocapture
  '
