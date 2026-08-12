#!/usr/bin/env bash
# Shared helpers for courts running INSIDE the guest VM.
#
# Everything here operates on the guest: guest /dev/uinput, guest X/Wayland,
# guest compositor. The host is never touched (rules 1, 51).

set -u

COURT_USER="${COURT_USER:-court}"
COURT_HOME="/home/$COURT_USER"
OUT="$COURT_HOME/court-output"
PAYLOAD="$COURT_HOME/payload"
RUN_ID="${RUN_ID:-vm}"
COURT_NAME="$(basename "$(dirname "$0")")"
export TARGET_SOCKET="${TARGET_SOCKET:-/tmp/ferrokey-test-target.sock}"
export DISPLAY="${DISPLAY:-:0}"

mkdir -p "$OUT"
EVENTS="$OUT/events.log"
: > "$EVENTS"

PASS=0
FAILURES=0

ok()   { PASS=$((PASS+1)); echo "  ok: $1"; }
bad()  { FAILURES=$((FAILURES+1)); echo "  FAIL: $1"; }

assert_eq() { # assert_eq <label> <actual> <expected>
    if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi
}

assert_contains() { # assert_contains <label> <file> <pattern>
    if grep -q "$3" "$2" 2>/dev/null; then ok "$1"; else bad "$1 (pattern '$3' not in $2)"; fi
}

# ---------------------------------------------------------------------------
# Receipt (rule 38) — written by each court at the end.
# ---------------------------------------------------------------------------
finish_court() { # finish_court [PASS|FAIL] [key value ...]
    # An explicit verdict (used by early-exit failures) is respected; otherwise
    # the result derives from the counters so accumulated failures can never be
    # masked by a hard-coded PASS.
    local result="$1"
    case "$result" in
        PASS|FAIL) shift ;;
        *) result=PASS; [ "$FAILURES" -eq 0 ] || result=FAIL ;;
    esac
    local extra_json="{}"
    if [ "$#" -gt 0 ]; then
        extra_json=$(python3 - "$@" <<'EOF'
import json, sys
args = sys.argv[1:]
kv = {args[i]: args[i + 1] for i in range(0, len(args), 2)}
print(json.dumps(kv))
EOF
)
    fi
    jq -n \
        --arg court "$COURT_NAME" \
        --arg result "$result" \
        --arg run_id "$RUN_ID" \
        --arg kernel "$(uname -r)" \
        --arg distro "$(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME" || echo unknown)" \
        --argjson extra "$extra_json" \
        '{court: $court, result: $result, run_id: $run_id, kernel: $kernel, distro: $distro} + $extra' \
        > "$OUT/receipt.json"
    echo "COURT $COURT_NAME: $result (${PASS} ok, ${FAILURES} fail)"
    [ "$result" = "PASS" ] && exit 0 || exit 1
}

# ---------------------------------------------------------------------------
# X11 session (rule 8: headless, dummy driver — never the host display).
# ---------------------------------------------------------------------------
start_xorg() {
    DISPLAY_X="${DISPLAY#:}"
    DISPLAY_X="${DISPLAY_X%%[^0-9]*}"
    pkill -f "Xorg :$DISPLAY_X" 2>/dev/null || true
    sleep 1
    sudo -u "$COURT_USER" Xorg ":$DISPLAY_X" -noreset -nolisten tcp \
        >"$OUT/xorg.log" 2>&1 &
    sleep 2
    if xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
        ok "Xorg started on $DISPLAY"
    else
        bad "Xorg failed to start on $DISPLAY"
        tail -5 "$OUT/xorg.log"
        finish_court FAIL "phase" "xorg-start"
    fi

    # A real EWMH window manager (rule 12/16: genuine desktop semantics). The
    # X11 focus courts must exercise WM_HINTS/EWMH against a real WM — a bare
    # X server has no focus policy at all, so `xdotool windowactivate` and the
    # OSK's `WM_HINTS.input=False` behaviour would be inert without one.
    if command -v openbox >/dev/null 2>&1; then
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" openbox >"$OUT/openbox.log" 2>&1 &
        sleep 1
        ok "openbox window manager started"
    else
        echo "openbox not installed; focus semantics degraded" >>"$OUT/openbox.log"
    fi
}

# ---------------------------------------------------------------------------
# Target applications (rule 18): machine-readable state on a socket.
# ---------------------------------------------------------------------------
start_target_x11() {
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" TARGET_SOCKET="$TARGET_SOCKET" \
        "$PAYLOAD/bin/ferrokey-test-target-x11" >"$OUT/target.log" 2>&1 &
    TARGET_PID=$!
    wait_target_ready
}

start_target_gtk() {
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" TARGET_SOCKET="$TARGET_SOCKET" \
        "$PAYLOAD/bin/ferrokey-test-target-gtk" >"$OUT/target.log" 2>&1 &
    TARGET_PID=$!
    wait_target_ready
}

start_target_qt() {
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" TARGET_SOCKET="$TARGET_SOCKET" \
        QT_QPA_PLATFORM=xcb "$PAYLOAD/bin/ferrokey-test-target-qt" >"$OUT/target.log" 2>&1 &
    TARGET_PID=$!
    wait_target_ready
}

start_target_slint() {
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" TARGET_SOCKET="$TARGET_SOCKET" \
        "$PAYLOAD/bin/ferrokey-test-target-slint" >"$OUT/target.log" 2>&1 &
    TARGET_PID=$!
    wait_target_ready
}

start_target_wayland() {
    sudo -u "$COURT_USER" env WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-court-0}" \
        XDG_RUNTIME_DIR=/run/user/1000 TARGET_SOCKET="$TARGET_SOCKET" \
        "$PAYLOAD/bin/ferrokey-test-target-wayland" >"$OUT/target.log" 2>&1 &
    TARGET_PID=$!
    wait_target_ready
}

wait_target_ready() {
    for _ in $(seq 1 50); do
        if grep -q '"event":"ready"' "$EVENTS" 2>/dev/null; then
            ok "target ready"
            return 0
        fi
        sleep 0.2
    done
    bad "target did not become ready"
    tail -5 "$OUT/target.log"
    finish_court FAIL "phase" "target-ready"
}

# Give the target window activation AND click into its input field so the
# editable widget definitely holds keyboard focus (Slint only focuses a
# TextInput on pointer press; GTK/Qt focus their default widget on activation
# but the click makes every toolkit deterministic).
focus_target() {
    local win
    win=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name ferrokey-test-target | head -1)
    [ -n "$win" ] || { bad "target window not found"; return 1; }
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool windowactivate --sync "$win" 2>/dev/null || true
    sleep 0.5
    # Click at the horizontal centre, ~2/3 down the window (the input field).
    eval "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$win")"
    local cx=$((X + WIDTH / 2)) cy=$((Y + HEIGHT * 2 / 3))
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$cx" "$cy" click 1
    sleep 0.5
}

# ---------------------------------------------------------------------------
# Receiver: connect to the target socket and log JSON events (rule 17:
# machine-readable oracles, not screenshots).
# ---------------------------------------------------------------------------
start_recorder() {
    nohup python3 "$PAYLOAD/courts/recv-events.py" "$TARGET_SOCKET" >> "$EVENTS" 2>"$OUT/recorder.err" &
    RECORDER_PID=$!
    sleep 0.5
}

wait_event() { # wait_event <pattern> <timeout-s>
    local pat="$1" timeout="${2:-10}"
    for _ in $(seq 1 $((timeout * 5))); do
        if grep -q "$pat" "$EVENTS" 2>/dev/null; then return 0; fi
        sleep 0.2
    done
    return 1
}

wait_focus() { # wait_focus <timeout-s>
    if wait_event '"event":"focus","focused":true' "${1:-10}"; then
        ok "target gained keyboard focus"
        return 0
    fi
    bad "target never gained keyboard focus"
    return 1
}

# ---------------------------------------------------------------------------
# ferrokeyd + ferrokey (rules 10, 9).
# ---------------------------------------------------------------------------
start_ferrokeyd() {
    # The daemon is the ONLY privileged component: root in the VM owns
    # /dev/uinput; the UI stays unprivileged.
    sudo -u root env RUST_LOG=info \
        "$PAYLOAD/bin/ferrokeyd" --config "$PAYLOAD/fixtures/ferrokeyd.yaml" \
        >"$OUT/ferrokeyd.log" 2>&1 &
    FERROKEYD_PID=$!
    sleep 1
    if [ -S /run/court/ferrokeyd.sock ] || ls /tmp/ferrokeyd.sock >/dev/null 2>&1; then
        ok "ferrokeyd listening"
    else
        bad "ferrokeyd did not start"
        cat "$OUT/ferrokeyd.log"
        finish_court FAIL "phase" "ferrokeyd-start"
    fi
}

start_ferrokey() {
    # The UI is fully unprivileged and the courts already run as the court
    # user, so no sudo here: `$!` must be the actual ferrokey pid (a sudo
    # wrapper would fork the real process, and killing the wrapper would
    # orphan the UI with its daemon connection still open).
    env DISPLAY="$DISPLAY" WAYLAND_DISPLAY="" \
        XDG_RUNTIME_DIR=/tmp/court-runtime \
        "$PAYLOAD/bin/ferrokey" --config "$PAYLOAD/fixtures/ferrokey.yaml" \
        >"$OUT/ferrokey.log" 2>&1 &
    FERROKEY_PID=$!
    sleep 3
    if kill -0 "$FERROKEY_PID" 2>/dev/null; then
        ok "ferrokey UI running (pid $FERROKEY_PID)"
    else
        bad "ferrokey UI exited"
        cat "$OUT/ferrokey.log"
        finish_court FAIL "phase" "ferrokey-start"
    fi
}

# ---------------------------------------------------------------------------
# OSK key geometry (rule 13): click a physical key by name using xdotool.
# The OSK window is 920x342 at (0,0) on the dummy display.
# ---------------------------------------------------------------------------
click_osk_key() { # click_osk_key <key-name> [button: 1|2|3]
    click_osk_key_button "$1" "${2:-1}"
}

click_osk_key_button() { # click_osk_key_button <key-name> <button>
    local pos
    pos=$(python3 "$PAYLOAD/courts/osk-geometry.py" "$1") || {
        bad "unknown OSK key $1"
        return 1
    }
    local x="${pos%,*}" y="${pos#*,}" btn="$2"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$x" "$y" click "$btn"
    sleep 0.3
}

hold_osk_key() { # hold_osk_key <key-name> <hold-ms>
    local pos
    pos=$(python3 "$PAYLOAD/courts/osk-geometry.py" "$1") || return 1
    local x="${pos%,*}" y="${pos#*,}"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$x" "$y" mousedown 1
    sleep "$(awk "BEGIN{print $2/1000}")"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
    sleep 0.3
}

# ---------------------------------------------------------------------------
# Focus assertions (rule 50): focus_before == focus_after.
# ---------------------------------------------------------------------------
focus_before() {
    wc -l < "$EVENTS" > "$OUT/focus-before-line" 2>/dev/null || echo 0 > "$OUT/focus-before-line"
    grep -c '"event":"focus","focused":true' "$EVENTS" > "$OUT/focus-before-count" 2>/dev/null || true
}

focus_after() {
    grep -c '"event":"focus","focused":true' "$EVENTS" > "$OUT/focus-after-count" 2>/dev/null || true
    local before after bline loses
    before=$(cat "$OUT/focus-before-count" 2>/dev/null || echo 0)
    after=$(cat "$OUT/focus-after-count" 2>/dev/null || echo 0)
    bline=$(cat "$OUT/focus-before-line" 2>/dev/null || echo 0)
    # Strong invariant (rule 50): the target must never lose keyboard focus
    # during the whole OSK interaction — zero `focused:false` events after the
    # snapshot, not merely "focus came back at the end".
    loses=$(tail -n "+$((bline + 1))" "$EVENTS" 2>/dev/null | grep -c '"event":"focus","focused":false' || true)
    if [ "$after" -ge "$before" ] && [ "$after" -gt 0 ] && [ "$loses" = "0" ]; then
        ok "focus preserved (focus_before == focus_after, zero focus losses)"
    else
        bad "focus NOT preserved (before=$before after=$after loses=$loses)"
        tail -20 "$EVENTS"
    fi
}

# ---------------------------------------------------------------------------
# Guest device evidence (rule 9).
# ---------------------------------------------------------------------------
capture_devices() {
    {
        echo "=== /proc/bus/input/devices ==="
        cat /proc/bus/input/devices 2>/dev/null
        echo "=== /dev/input ==="
        ls -la /dev/input 2>/dev/null
    } > "$OUT/devices.txt"
}

# The /dev/input/eventN node of the Ferrokey virtual keyboard.
ferrokey_device_node() {
    capture_devices
    python3 - <<'EOF'
import re
sec = ""
found = ""
# Each device section starts at an "I: " line; the eventN node lives in the
# "H: Handlers=..." line which follows the name line, so scan the FULL
# section for the node, not just the lines before the name.
for line in open("/home/court/court-output/devices.txt"):
    line = line.rstrip()
    if line.startswith("I: "):
        if "Ferrokey Virtual Keyboard" in sec:
            m = re.search(r"event([0-9]+)", sec)
            if m:
                found = m.group(0)
        sec = ""
    sec += line + "\n"
if not found and "Ferrokey Virtual Keyboard" in sec:
    m = re.search(r"event([0-9]+)", sec)
    if m:
        found = m.group(0)
print(found)
EOF
}
