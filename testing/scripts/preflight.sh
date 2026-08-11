#!/usr/bin/env bash
# Host-safety preflight (rule 43). Aborts the whole court run on any
# violation. The host must be an orchestrator only.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env
host_safety_preflight
