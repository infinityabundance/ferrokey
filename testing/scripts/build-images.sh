#!/usr/bin/env bash
# Build the court images (rule 31 / 5): reproducible, from scratch.
#
#   ./testing/scripts/build-images.sh [--no-cache]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh

CACHE_FLAG=""
if [ "${1:-}" = "--no-cache" ]; then CACHE_FLAG="--no-cache"; fi

echo "==> building $BUILDER_IMAGE (builder)"
"$DOCKER" build $CACHE_FLAG -t "$BUILDER_IMAGE" -f docker/Dockerfile.builder docker/

echo "==> building $ORACLE_IMAGE (vm oracle)"
"$DOCKER" build $CACHE_FLAG -t "$ORACLE_IMAGE" -f docker/Dockerfile.oracle docker/

echo "==> building $TARGETS_IMAGE (court targets)"
"$DOCKER" build $CACHE_FLAG -t "$TARGETS_IMAGE" -f docker/Dockerfile.targets docker/

echo "court images built"
