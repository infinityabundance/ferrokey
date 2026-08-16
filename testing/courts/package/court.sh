#!/usr/bin/env bash
# PACKAGE.* — the packaging court (Phase 6).
#
# Applies the DOCUMENTED production install layout (PACKAGING/README.md) to
# the guest verbatim and verifies it, proves the installed broker serves a
# real client, and builds real distributable artifacts (.deb always — dpkg
# is stock on the Debian guest; .rpm when the rpm toolchain is available).
#
# Gates:
#   PACKAGE.001  product binaries land at the documented paths
#   PACKAGE.002  desktop entry + icon land at the documented paths
#   PACKAGE.003  production config at /etc/ferrokey (root-owned 0644, §45)
#   PACKAGE.004  systemd unit passes systemd-analyze verify
#   PACKAGE.005  the packaged broker serves a real client (court helper flow)
#   PACKAGE.006  installed binaries are byte-identical to the built artifacts
#   PACKAGE.007  a real .deb builds, installs cleanly, and lands the layout
#   PACKAGE.008  a real .rpm builds (best-effort toolchain) with the layout
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

PREFIX=/usr/local
SOCK=/run/ferrokeyd/ferrokeyd.sock
PKG_VERSION=0.1.6   # track the workspace release; the court pins the artifact

# ── 1. the documented install layout (PACKAGING/README.md) ───────────────────
# README: the daemon binary at /usr/bin/ferrokeyd; the UI binary at
# $PREFIX/bin/ferrokey; the desktop entry + icon under $PREFIX/share; the
# security-boundary config at /etc/ferrokey/ferrokeyd.yaml (root-owned 0644,
# §45); the hardened unit at /usr/lib/systemd/system.
sudo install -m 0755 "$PAYLOAD/bin/ferrokeyd" /usr/bin/ferrokeyd
sudo install -Dm 0755 "$PAYLOAD/bin/ferrokey" "$PREFIX/bin/ferrokey"
sudo install -Dm 0644 "$PAYLOAD/PACKAGING/ferrokey.desktop" \
    "$PREFIX/share/applications/ferrokey.desktop"
sudo install -Dm 0644 "$PAYLOAD/PACKAGING/icons/ferrokey.svg" \
    "$PREFIX/share/icons/hicolor/scalable/apps/ferrokey.svg"
sudo mkdir -p /etc/ferrokey
sudo install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.yaml" /etc/ferrokey/ferrokeyd.yaml
sudo install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.service" \
    /usr/lib/systemd/system/ferrokeyd.service
sudo chown root:root /etc/ferrokey/ferrokeyd.yaml
sudo chmod 0644 /etc/ferrokey/ferrokeyd.yaml

# ── PACKAGE.001: binaries at the documented paths ────────────────────────────
if [ -x /usr/bin/ferrokeyd ] && [ -x "$PREFIX/bin/ferrokey" ]; then
    ok "PACKAGE.001 binaries installed (/usr/bin/ferrokeyd, $PREFIX/bin/ferrokey)"
else
    bad "PACKAGE.001 binaries not at the documented paths"
fi

# ── PACKAGE.002: desktop entry + icon ────────────────────────────────────────
if [ -f "$PREFIX/share/applications/ferrokey.desktop" ] \
    && grep -q '^Exec=ferrokey$' "$PREFIX/share/applications/ferrokey.desktop" \
    && grep -q '^Icon=ferrokey$' "$PREFIX/share/applications/ferrokey.desktop" \
    && [ -s "$PREFIX/share/icons/hicolor/scalable/apps/ferrokey.svg" ]; then
    ok "PACKAGE.002 desktop entry + icon installed (Exec/Icon match)"
else
    bad "PACKAGE.002 desktop entry or icon missing/malformed"
fi

# ── PACKAGE.003: security-boundary config placement (§45) ────────────────────
if [ -f /etc/ferrokey/ferrokeyd.yaml ] \
    && [ "$(stat -c %U /etc/ferrokey/ferrokeyd.yaml)" = root ] \
    && [ "$(stat -c %a /etc/ferrokey/ferrokeyd.yaml)" = 644 ]; then
    ok "PACKAGE.003 config at /etc/ferrokey/ferrokeyd.yaml (root:root 0644)"
else
    bad "PACKAGE.003 config placement/permissions wrong"
fi

# ── PACKAGE.004: the installed unit passes systemd-analyze verify ────────────
if sudo systemd-analyze verify /usr/lib/systemd/system/ferrokeyd.service \
    >"$OUT/package-systemd-verify.log" 2>&1; then
    ok "PACKAGE.004 systemd-analyze verify clean"
else
    bad "PACKAGE.004 systemd-analyze verify FAILED"
    cat "$OUT/package-systemd-verify.log"
fi

# ── PACKAGE.005: the packaged broker serves a real client ────────────────────
# The court-helper flow (start_ferrokeyd) against the INSTALLED production
# config; the helper launches the payload binary, and PACKAGE.006 proves the
# installed binary is byte-identical, so this exercises the packaged layout.
start_ferrokeyd /etc/ferrokey/ferrokeyd.yaml
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
        handshake key-down 30 key-up 30 release-all; then
    ok "PACKAGE.005 packaged broker serves a real client (handshake + keys)"
else
    bad "PACKAGE.005 packaged broker failed to serve"
    cat "$OUT/ferrokeyd.log"
fi

# ── PACKAGE.006: installed binaries are byte-identical to the artifacts ──────
if [ "$(sha256sum /usr/bin/ferrokeyd | cut -d' ' -f1)" = \
     "$(sha256sum "$PAYLOAD/bin/ferrokeyd" | cut -d' ' -f1)" ] \
    && [ "$(sha256sum "$PREFIX/bin/ferrokey" | cut -d' ' -f1)" = \
         "$(sha256sum "$PAYLOAD/bin/ferrokey" | cut -d' ' -f1)" ]; then
    ok "PACKAGE.006 installed binaries byte-identical to the built artifacts"
else
    bad "PACKAGE.006 installed binaries differ from the artifacts"
fi

# ── PACKAGE.007: a real .deb artifact ────────────────────────────────────────
DEBROOT=/tmp/ferrokey-deb/ferrokey_${PKG_VERSION}_amd64
rm -rf /tmp/ferrokey-deb
mkdir -p "$DEBROOT/DEBIAN" "$DEBROOT/usr/bin" \
    "$DEBROOT/usr/share/applications" \
    "$DEBROOT/usr/share/icons/hicolor/scalable/apps" \
    "$DEBROOT/usr/lib/systemd/system" "$DEBROOT/etc/ferrokey"
install -m 0755 "$PAYLOAD/bin/ferrokeyd" "$DEBROOT/usr/bin/ferrokeyd"
install -m 0755 "$PAYLOAD/bin/ferrokey" "$DEBROOT/usr/bin/ferrokey"
install -m 0644 "$PAYLOAD/PACKAGING/ferrokey.desktop" \
    "$DEBROOT/usr/share/applications/ferrokey.desktop"
install -m 0644 "$PAYLOAD/PACKAGING/icons/ferrokey.svg" \
    "$DEBROOT/usr/share/icons/hicolor/scalable/apps/ferrokey.svg"
install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.service" \
    "$DEBROOT/usr/lib/systemd/system/ferrokeyd.service"
install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.yaml" \
    "$DEBROOT/etc/ferrokey/ferrokeyd.yaml"
cat > "$DEBROOT/DEBIAN/control" <<EOF
Package: ferrokey
Version: $PKG_VERSION
Section: utils
Priority: optional
Architecture: amd64
Maintainer: infinityabundance <255699974+infinityabundance@users.noreply.github.com>
Depends: libc6
Description: On-screen keyboard with terminal workspace
 Ferrokey: an on-screen keyboard that preserves target focus via
 kernel-level input injection, with an embedded terminal workspace.
 The ferrokeyd broker provides the constrained /dev/uinput bridge.
EOF
sudo dpkg-deb --build "$DEBROOT" "/tmp/ferrokey_${PKG_VERSION}_amd64.deb" >/dev/null
# Capture first (a `grep -q` pipe would SIGPIPE dpkg-deb and trip pipefail).
DEB_INFO=$(sudo dpkg-deb --info "/tmp/ferrokey_${PKG_VERSION}_amd64.deb" 2>/dev/null)
DEB_CONTENTS=$(sudo dpkg-deb --contents "/tmp/ferrokey_${PKG_VERSION}_amd64.deb" 2>/dev/null)
if [ -s "/tmp/ferrokey_${PKG_VERSION}_amd64.deb" ] \
    && [ "$(printf '%s' "$DEB_INFO" | grep -c '^ Package: ferrokey$' || true)" -gt 0 ] \
    && [ "$(printf '%s' "$DEB_CONTENTS" | grep -c 'usr/bin/ferrokeyd$' || true)" -gt 0 ] \
    && [ "$(printf '%s' "$DEB_CONTENTS" | grep -c 'etc/ferrokey/ferrokeyd.yaml$' || true)" -gt 0 ]; then
    ok "PACKAGE.007 .deb built and structurally verified"
else
    bad "PACKAGE.007 .deb build/verification failed"
fi
# Install the artifact itself and confirm dpkg owns the documented layout.
if sudo dpkg -i "/tmp/ferrokey_${PKG_VERSION}_amd64.deb" >/dev/null 2>&1 \
    && dpkg -s ferrokey 2>/dev/null | grep -q "Status: install ok installed" \
    && [ -x /usr/bin/ferrokeyd ] && [ -x /usr/bin/ferrokey ] \
    && [ -f /usr/share/applications/ferrokey.desktop ] \
    && [ -f /etc/ferrokey/ferrokeyd.yaml ]; then
    ok "PACKAGE.007b .deb installs cleanly; dpkg owns the documented layout"
else
    bad "PACKAGE.007b .deb install failed"
    sudo dpkg -s ferrokey 2>/dev/null || true
fi

# ── PACKAGE.008: a real .rpm artifact (best-effort toolchain) ────────────────
# rpm is not stock on the Debian guest; install it once (mirroring a release
# pipeline that provides the toolchain) — binutils for the brp-strip step.
# If unavailable, the gate is skipped with the reason recorded — dpkg is the
# primary artifact on this guest.
if sudo apt-get install -y --no-install-recommends rpm binutils >/dev/null 2>&1 \
    && command -v rpmbuild >/dev/null 2>&1; then
    rm -rf /tmp/rpmstage /tmp/rpmbuild
    mkdir -p /tmp/rpmstage/usr/bin /tmp/rpmstage/usr/share/applications \
        /tmp/rpmstage/usr/share/icons/hicolor/scalable/apps \
        /tmp/rpmstage/usr/lib/systemd/system /tmp/rpmstage/etc/ferrokey
    install -m 0755 "$PAYLOAD/bin/ferrokeyd" /tmp/rpmstage/usr/bin/ferrokeyd
    install -m 0755 "$PAYLOAD/bin/ferrokey" /tmp/rpmstage/usr/bin/ferrokey
    install -m 0644 "$PAYLOAD/PACKAGING/ferrokey.desktop" \
        /tmp/rpmstage/usr/share/applications/ferrokey.desktop
    install -m 0644 "$PAYLOAD/PACKAGING/icons/ferrokey.svg" \
        /tmp/rpmstage/usr/share/icons/hicolor/scalable/apps/ferrokey.svg
    install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.service" \
        /tmp/rpmstage/usr/lib/systemd/system/ferrokeyd.service
    install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.yaml" /tmp/rpmstage/etc/ferrokey/ferrokeyd.yaml
    cat > /tmp/ferrokey.spec <<EOF
Name: ferrokey
Version: $PKG_VERSION
Release: 1
Summary: On-screen keyboard with terminal workspace
License: MIT OR Apache-2.0
BuildArch: x86_64

%description
Ferrokey: an on-screen keyboard that preserves target focus via kernel-level
input injection, with an embedded terminal workspace. The ferrokeyd broker
provides the constrained /dev/uinput bridge.

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
cp -a /tmp/rpmstage/. %{buildroot}/

%files
/usr/bin/ferrokey
/usr/bin/ferrokeyd
/usr/share/applications/ferrokey.desktop
/usr/share/icons/hicolor/scalable/apps/ferrokey.svg
/usr/lib/systemd/system/ferrokeyd.service
/etc/ferrokey/ferrokeyd.yaml
EOF
    if sudo rpmbuild -bb /tmp/ferrokey.spec \
        --define "_topdir /tmp/rpmbuild" \
        --define "_enable_debug_package 0" \
        --define "debug_package %{nil}" >"$OUT/rpmbuild.log" 2>&1; then
        RPM=$(ls /tmp/rpmbuild/RPMS/*/ferrokey-*.rpm 2>/dev/null | head -1)
        RPM_FILES=$(sudo rpm -qlp "$RPM" 2>/dev/null)
        if [ -n "$RPM" ] \
            && [ "$(printf '%s' "$RPM_FILES" | grep -c '^/usr/bin/ferrokeyd$' || true)" -gt 0 ] \
            && [ "$(printf '%s' "$RPM_FILES" | grep -c '^/etc/ferrokey/ferrokeyd.yaml$' || true)" -gt 0 ]; then
            ok "PACKAGE.008 .rpm built and structurally verified ($(basename "$RPM"))"
        else
            bad "PACKAGE.008 .rpm contents missing expected layout"
        fi
    else
        bad "PACKAGE.008 rpmbuild failed"
        cat "$OUT/rpmbuild.log"
    fi
else
    echo "  PACKAGE.008 skipped: rpm toolchain unavailable on this guest"
    echo "  PACKAGE.008 skipped: rpm toolchain unavailable" >> "$ASSERTIONS"
fi

finish_court "court" "package"
