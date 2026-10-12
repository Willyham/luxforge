#!/usr/bin/env bash
# Brings the reference stack up under a throwaway compose project with throwaway credentials, runs a
# test against it, and removes every container, network and volume it created.
#   services/usage-statistics/test.sh                   the stack test, tests/stack_test.py
#   services/usage-statistics/test.sh --client NEW_DIR  the desktop against the stack, tests/client_test.py:
#                                                       builds the release editor, runs the xtask
#                                                       `usage-backend` scenario into NEW_DIR/smoke, and
#                                                       checks what the stack stored and logged
# Needs Docker with the compose plugin, and python3. The first run builds the ingress (Rust) and
# Grafana images, which takes a few minutes.
set -euo pipefail
cd "$(dirname "$0")"

mode=stack
if [ "${1:-}" = "--client" ]; then
  mode=client
  out="${2:?--client needs a new output directory}"
  case "$out" in /*) ;; *) out="$OLDPWD/$out" ;; esac
  if [ -e "$out" ]; then echo "test.sh: $out already exists" >&2; exit 2; fi
elif [ $# -gt 0 ]; then
  echo "usage: test.sh [--client NEW_DIR]" >&2; exit 2
fi

project="lfusagetest$(date +%s)$$"
work="$(mktemp -d)"
port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
secret() { python3 -I -c 'import secrets; print(secrets.token_hex(12))'; }

# The stack test turns the per-installation limiter off (it sends many batches per installation
# and tests the limiter with the probe); the client test needs it on, one batch per 5 seconds with
# no burst, so the desktop's second batch in quick succession meets a 429 (the scenario's
# ID_INTERVAL_S).
if [ "$mode" = client ]; then id_interval=5; id_burst=1; else id_interval=0; id_burst=5; fi

cat > "$work/env" <<EOF
LUXFORGE_CLICKHOUSE_ADMIN_PASSWORD=$(secret)
LUXFORGE_CLICKHOUSE_WRITER_PASSWORD=$(secret)
LUXFORGE_CLICKHOUSE_READER_PASSWORD=$(secret)
LUXFORGE_GRAFANA_ADMIN_USER=luxforge-test-admin
LUXFORGE_GRAFANA_ADMIN_PASSWORD=$(secret)
LUXFORGE_INGRESS_PORT=$port
LUXFORGE_INGRESS_ID_INTERVAL_SECS=$id_interval
LUXFORGE_INGRESS_ID_BURST=$id_burst
LUXFORGE_INGRESS_STATS_INTERVAL_SECS=5
LUXFORGE_UPDATE_STORE=$work/update-store
EOF
mkdir "$work/update-store"

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
if [ "$mode" = client ]; then
  python3 -I tests/client_test.py "$project" "$work/env" "$port" "$out" || status=$?
else
  python3 -I tests/stack_test.py "$project" "$work/env" "$port" || status=$?
fi
exit "$status"
