#!/usr/bin/env bash
# Build the court images (rule 31 / 5): reproducible, from scratch.
#
#   ./testing/scripts/build-images.sh [--no-cache] [builder|oracle|targets|kani ...]
#
# With no image args all four are built. A subset can be selected: the proof
# scripts rebuild only the kani image on demand — never the whole set (OOM
# limits: an unnecessary full rebuild refills the docker build cache on the
# tmpfs data-root).
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh

CACHE_FLAG=""
if [ "${1:-}" = "--no-cache" ]; then
    CACHE_FLAG="--no-cache"; shift
fi
# COURT_NETWORK: "bridge" on normal hosts, "host" where bridge veths are
# unavailable (e.g. restricted sandboxes). Host networking is safe here:
# the build containers only reach package registries, and the court env is
# explicitly sanitized.
NETWORK="${COURT_NETWORK:-bridge}"
WANT=("$@")
if [ "${#WANT[@]}" -eq 0 ]; then
    WANT=(builder oracle targets kani)
fi

for img in "${WANT[@]}"; do
    case "$img" in
        builder)
            echo "==> building $BUILDER_IMAGE (builder, network=$NETWORK)"
            "$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$BUILDER_IMAGE" -f docker/Dockerfile.builder docker/
            ;;
        oracle)
            echo "==> building $ORACLE_IMAGE (vm oracle, network=$NETWORK)"
            "$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$ORACLE_IMAGE" -f docker/Dockerfile.oracle docker/
            ;;
        targets)
            echo "==> building $TARGETS_IMAGE (court targets, network=$NETWORK)"
            "$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$TARGETS_IMAGE" -f docker/Dockerfile.targets docker/
            ;;
        kani)
            echo "==> building $KANI_IMAGE (kani proofs, network=$NETWORK)"
            "$DOCKER" build $CACHE_FLAG --network "$NETWORK" -t "$KANI_IMAGE" -f docker/Dockerfile.kani docker/
            ;;
        *)
            echo "unknown image '$img' (expected: builder|oracle|targets|kani)" >&2
            exit 1
            ;;
    esac
done

echo "court images built"
