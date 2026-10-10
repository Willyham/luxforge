#!/usr/bin/env bash
# Brings the reference stack up under a throwaway compose project with throwaway credentials, runs
# tests/stack_test.py against it, and removes every container, network and volume it created.
#   services/usage-statistics/test.sh            (needs Docker with the compose plugin, and python3)
# The first run builds the ingress (Rust) and Grafana images, which takes a few minutes.
set -euo pipefail
cd "$(dirname "$0")"

project="lfusagetest$(date +%s)$$"
work="$(mktemp -d)"
port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
secret() { python3 -I -c 'import secrets; print(secrets.token_hex(12))'; }

cat > "$work/env" <<EOF
LUXFORGE_CLICKHOUSE_ADMIN_PASSWORD=$(secret)
LUXFORGE_CLICKHOUSE_WRITER_PASSWORD=$(secret)
LUXFORGE_CLICKHOUSE_READER_PASSWORD=$(secret)
LUXFORGE_GRAFANA_ADMIN_USER=luxforge-test-admin
LUXFORGE_GRAFANA_ADMIN_PASSWORD=$(secret)
LUXFORGE_INGRESS_PORT=$port
LUXFORGE_INGRESS_ID_INTERVAL_SECS=0
LUXFORGE_INGRESS_ID_BURST=5
LUXFORGE_INGRESS_STATS_INTERVAL_SECS=5
EOF

cleanup() {
  docker compose --env-file "$work/env" -p "$project" down --volumes --remove-orphans --timeout 5 >/dev/null 2>&1 || true
  rm -rf "$work"
  left="$(docker ps -a --filter "label=com.docker.compose.project=$project" -q | wc -l | tr -d ' ')"
  left="$((left + $(docker volume ls --filter "label=com.docker.compose.project=$project" -q | wc -l)))"
  left="$((left + $(docker network ls --filter "label=com.docker.compose.project=$project" -q | wc -l)))"
  echo "cleanup: $left containers, volumes or networks of project $project remain"
}
trap cleanup EXIT

status=0
python3 -I tests/stack_test.py "$project" "$work/env" "$port" || status=$?
exit "$status"
