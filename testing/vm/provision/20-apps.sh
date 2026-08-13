#!/usr/bin/env bash
# Application-matrix provisioning (runs only while baking the `debian-12-browsers`
# image): downloads the pinned Electron runtime into /opt/electron.
#
# Debian has no `electron` package in bookworm, so the release zip is fetched
# once at image-build time, checksum-verified, and cached in the immutable
# base image — never during court execution (rule 30: no network during tests).
#
# Pinned: Electron v31.7.7 linux-x64.
set -euo pipefail

ELECTRON_VERSION="31.7.7"
ELECTRON_URL="https://github.com/electron/electron/releases/download/v${ELECTRON_VERSION}/electron-v${ELECTRON_VERSION}-linux-x64.zip"
ELECTRON_SHA256="00a2e8e5f52fe39c37cfc9d7bd7629e560017d28ee94c51495bf7e39c84b2d47"
MARKER=/var/lib/ferrokey-apps-provisioned

if [ -x /opt/electron/electron ]; then
    touch "$MARKER"
    echo "electron already provisioned"
    exit 0
fi

mkdir -p /opt/electron /tmp/electron-dl
cd /tmp/electron-dl
curl -fL --retry 3 -o electron.zip "$ELECTRON_URL"
echo "$ELECTRON_SHA256  electron.zip" | sha256sum -c -
unzip -q electron.zip -d /opt/electron
chmod +x /opt/electron/electron /opt/electron/chrome-sandbox
rm -rf /tmp/electron-dl

/opt/electron/electron --version > /var/lib/ferrokey-electron-version.txt 2>&1 || true
touch "$MARKER"
echo "electron provisioning complete"
