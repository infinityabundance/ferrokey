#!/usr/bin/env bash
# Debug probe: runs INSIDE the court VM after the court finishes, dumping
# the guest state that explains Wayland-court geometry/focus failures.
set +e
cd ~/court-output 2>/dev/null || cd ~/payload
echo "== assertions:"
cat ~/court-output/assertions.log 2>/dev/null
echo "== events.log:"
cat ~/court-output/events.log 2>/dev/null
echo "== compositor output window:"
SWAY_WIN=$(DISPLAY=:0 xwininfo -root -children 2>/dev/null | awk 'NR >= 7 && $1 ~ /^0x[0-9a-f]+$/ {print $1; exit}')
echo "sway window id: $SWAY_WIN"
if [ -n "$SWAY_WIN" ]; then
    DISPLAY=:0 xwininfo -id "$SWAY_WIN" 2>/dev/null | grep -E "Absolute upper-left|Width|Height"
    DISPLAY=:0 xdotool getwindowgeometry --shell "$SWAY_WIN" 2>/dev/null
fi
echo "== cursor + grid probe (screen -> surface-local):"
DISPLAY=:0 xdotool getmouselocation
for pt in "200 200" "800 300" "500 700" "300 500" "900 700"; do
    set -- $pt
    echo "-- move to $1,$2"
    DISPLAY=:0 xdotool mousemove "$1" "$2"
    sleep 1
    DISPLAY=:0 xdotool getmouselocation
done
echo "== ferrokey pointer lines:"
grep "layer pointer" ~/court-output/ferrokey.log 2>/dev/null | tail -30
echo "== XWayland OSK window state (on :1):"
for d in 0 1; do
    echo "-- display :$d ferrokey windows:"
    DISPLAY=:$d xdotool search --name 'Ferrokey' 2>/dev/null | while read -r w; do
        echo "   win $w:"
        DISPLAY=:$d xdotool getwindowgeometry --shell "$w" 2>/dev/null | sed 's/^/     /'
        DISPLAY=:$d xprop -id "$w" WM_NAME WM_HINTS _NET_WM_WINDOW_TYPE _NET_WM_STATE 2>/dev/null | sed 's/^/     /'
    done
done
echo "== ferrokey.log (xwayland backend lines):"
grep -E "x11|X11|surface|backend|WM_HINTS|focus" ~/court-output/ferrokey.log 2>/dev/null | tail -15
echo "== sway.log tail:"
tail -8 ~/court-output/sway.log 2>/dev/null
echo "== wayland sockets:"
ls /run/user/1000/ 2>/dev/null
echo "== target process:"
pgrep -af ferrokey-test-target || echo none
echo "== sway tree via IPC:"
IPC=$(ls /run/user/1000/sway-ipc.*.sock 2>/dev/null | head -1)
if [ -n "$IPC" ]; then
    SWAYSOCK="$IPC" swaymsg -t get_tree 2>/dev/null | python3 -c "
import json, sys
t = json.load(sys.stdin)
def walk(n, d=0):
    r = n.get('rect'); ty = n.get('type'); app = n.get('app_id') or n.get('name')
    if r is not None and (ty or app):
        print('  '*d, ty, repr(app), r, 'focused=', n.get('focused'))
    for c in n.get('nodes', []) + n.get('floating_nodes', []):
        walk(c, d+1)
for n in t.get('nodes', []):
    walk(n)
" 2>&1
fi
echo "== done"
