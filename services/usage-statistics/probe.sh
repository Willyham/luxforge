#!/usr/bin/env bash
# Synthetic live-endpoint probe for a usage statistics ingress. All the logic is in probe.py; this
# wrapper only runs it with python3 in isolated mode.
#   ./probe.sh http://127.0.0.1:8080                      a local reference stack
#   ./probe.sh --strict https://HOST                      every check; see DEPLOYMENT.md
# It sends synthetic data only and refuses the production domain without --confirm-production.
set -euo pipefail
exec python3 -I "$(dirname "$0")/probe.py" "$@"
