#!/usr/bin/env bash
# Sends one synthetic batch (random IDs, no real data) to a usage statistics ingress and prints the
# HTTP status. 202 means the batch was accepted. A probe sends request headers of its own only
# (curl's), so it is safe to run against a deployment whose logs you are checking.
#   ./probe.sh http://127.0.0.1:8080/v1/usage/batches
set -euo pipefail
url="${1:?usage: probe.sh URL}"
id() { uuidgen | tr 'A-Z' 'a-z'; }
body=$(cat <<EOF
{"schema":1,"disclosure_version":1,"release":"0.0.0+000000000000","installation_id":"$(id)","session_id":"$(id)",
 "session_start":"$(date -u +%Y-%m-%dT%H:%M:%SZ)","batch_seq":0,"dropped":0,
 "device":{"os":{"family":"other","version":[0]},"arch":"other",
           "cpu":{"vendor":"other","physical_cores":1,"logical_cores":1},
           "memory_gib_band":8,"gpus":[],"renderer":"reference","renderer_fallback":"other"},
 "events":[{"seq":0,"t":0,"name":"session.start","trigger":"launch"}]}
EOF
)
curl -sS -o /dev/null -w '%{http_code}\n' -X POST -H 'Content-Type: application/json' --data "$body" "$url"
