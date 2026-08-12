#!/usr/bin/env bash
# One-off debug: use libX11 to PutImage a depth-24 ZPixmap and report the
# exact per-row byte count the wire format uses.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug8-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey8*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey8
PUB=$(cat $STATE/keys/debugkey8.pub)

cat > $STATE/seeds/debug8-userdata.yaml <<EOF
#cloud-config
ssh_pwauth: false
disable_root: false
users:
  - name: court
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: [sudo]
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - "$PUB"
package_update: true
packages: [xserver-xorg-video-dummy, xserver-xorg-core, x11-utils, build-essential, libx11-dev, jq, procps, python3]
write_files:
  - path: /etc/X11/Xwrapper.config
    content: |
      allowed_users=anybody
      needs_root_rights=yes
  - path: /etc/X11/xorg.conf.d/99-ferrokey-dummy.conf
    content: |
      Section "Device"
          Identifier  "DummyDevice"
          Driver      "dummy"
          VideoRam    65536
      EndSection
      Section "Screen"
          Identifier  "DummyScreen"
          Device      "DummyDevice"
          DefaultDepth 24
          SubSection "Display"
              Depth 24
              Modes "1280x720"
          EndSubSection
      EndSection
EOF
cloud-localds $STATE/seeds/debug8-seed.iso $STATE/seeds/debug8-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug8-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22992-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug8.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey8 -p 22992 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey8 -p 22992 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'

cat > /tmp/probe.c <<'CEOF'
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "no display\n"); return 1; }
    int screen = DefaultScreen(d);
    Window win = XCreateSimpleWindow(d, RootWindow(d, screen), 0, 0, 200, 100, 0,
                                     BlackPixel(d, screen), WhitePixel(d, screen));
    XMapWindow(d, win);
    XSync(d, False);

    XImage *img = XCreateImage(d, DefaultVisual(d, screen), 24, ZPixmap, 0,
                               (char *)malloc(200 * 100 * 4), 200, 100, 32, 0);
    printf("depth=%d bpp=%d bytes_per_line=%d\n", img->depth, img->bits_per_pixel, img->bytes_per_line);
    memset(img->data, 0x40, (size_t)img->bytes_per_line * 100);
    XPutImage(d, win, DefaultGC(d, screen), img, 0, 0, 0, 0, 200, 100);
    XSync(d, False);
    printf("PutImage OK (no async error before XSync)\n");
    /* try a big one: 920x342, same 24-bit image */
    XImage *big = XCreateImage(d, DefaultVisual(d, screen), 24, ZPixmap, 0,
                               (char *)malloc(920 * 342 * 4), 920, 342, 32, 0);
    printf("big: depth=%d bpp=%d bytes_per_line=%d\n", big->depth, big->bits_per_pixel, big->bytes_per_line);
    memset(big->data, 0x40, (size_t)big->bytes_per_line * 342);
    XPutImage(d, win, DefaultGC(d, screen), big, 0, 0, 0, 0, 920, 342);
    XSync(d, False);
    printf("Big PutImage OK\n");
    XDestroyImage(img);
    XDestroyImage(big);
    XCloseDisplay(d);
    return 0;
}
CEOF
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey8 -p 22992 court@127.0.0.1 'cat > /tmp/probe.c' < /tmp/probe.c
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey8 -p 22992 court@127.0.0.1 'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo | grep "depth of root"; gcc -o /tmp/probe /tmp/probe.c -lX11 && DISPLAY=:0 timeout 5 /tmp/probe 2>&1' || true

qemu_pid=$(cat /court/state/qemu-debug8.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug8 done"
