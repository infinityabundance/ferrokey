#!/usr/bin/env bash
# §66–§68: build (once) the KASAN+UBSAN+LOCKDEP+debug kernel used by the
# kernel-debug court. The artifacts (bzImage, config, System.map) are cached
# in the ferrokey-kasan-kernel volume so the slow build runs only when the
# volume is empty or missing.
#
#   ./testing/scripts/build-kasan-kernel.sh
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

NETWORK="${COURT_NETWORK:-bridge}"

if "$DOCKER" volume inspect ferrokey-kasan-kernel >/dev/null 2>&1; then
    if "$DOCKER" run --rm -v ferrokey-kasan-kernel:/k:ro alpine sh -c \
        '[ -f /k/bzImage ] && [ -f /k/config ] && [ -s /k/bzImage ]'; then
        echo "KASAN kernel already built (cached in ferrokey-kasan-kernel)"
        exit 0
    fi
fi

"$DOCKER" volume create ferrokey-kasan-kernel >/dev/null 2>&1 || true
echo "==> building the KASAN/debug kernel (one-time; 20-40 min on 16 cores)"
"$DOCKER" build --network "$NETWORK" -t ferrokey-kasan:latest -f docker/Dockerfile.kasan docker/

"$DOCKER" run --rm -v ferrokey-kasan-kernel:/out ferrokey-kasan:latest \
    sh -c 'cp /artifacts/bzImage /artifacts/config /artifacts/System.map /out/'

# The build tree is only needed to produce the artifacts; drop the image and
# its dangling layers so the tmpfs-backed data-root does not retain ~6 GB.
"$DOCKER" rmi ferrokey-kasan:latest >/dev/null 2>&1 || true
"$DOCKER" image prune -f >/dev/null 2>&1 || true

"$DOCKER" run --rm -v ferrokey-kasan-kernel:/k:ro alpine sh -c \
    'ls -la /k; grep -E "CONFIG_KASAN=y|CONFIG_UBSAN=y|CONFIG_PROVE_LOCKING=y" /k/config' \
    || true
echo "KASAN kernel ready"
