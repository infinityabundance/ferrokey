#!/usr/bin/env bash
# Build the court images (rule 31 / 5): reproducible, from scratch.
#
#   ./testing/scripts/build-images.sh [--no-cache]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh

CACHE_FLAG=""
if [ "${1:-}" = "--no-cache" ]; then CACHE_FLAG="--no-cache"; fi
# COURT_NETWORK: "bridge" on normal hosts, "host" where bridge veths are
# unavailable (e.g. restricted sandboxes). Host networking is safe here:
# the build containers only reach package registries, and the court env is
# explicitly sanitized.
NETWORK="${COURT_NETWORK:-bridge}"

echo "==> building $BUILDER_IMAGE (builder, network=$NETWORK)"
"$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$BUILDER_IMAGE" -f docker/Dockerfile.builder docker/

echo "==> building $ORACLE_IMAGE (vm oracle, network=$NETWORK)"
"$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$ORACLE_IMAGE" -f docker/Dockerfile.oracle docker/

echo "==> building $TARGETS_IMAGE (court targets, network=$NETWORK)"
"$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$TARGETS_IMAGE" -f docker/Dockerfile.targets docker/

echo "==> building $KANI_IMAGE (kani proofs, network=$NETWORK)"
"$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$KANI_IMAGE" -f docker/Dockerfile.kani docker/

echo "court images built"
