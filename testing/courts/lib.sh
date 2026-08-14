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

# Per-assertion receipt (rule 47 / addendum §37): every ok/bad is appended as
# one machine-readable line so the compatibility receipt is GENERATED from
# evidence and never hand-edited. Newlines in labels are flattened.
ASSERTIONS="$OUT/assertions.log"
: > "$ASSERTIONS"

PASS=0
FAILURES=0

ok() {
    PASS=$((PASS+1))
    echo "  ok: $1"
    echo "PASS $(printf '%s' "$1" | tr '\n' ' ')" >> "$ASSERTIONS"
}
bad() {
    FAILURES=$((FAILURES+1))
    echo "  FAIL: $1"
    echo "FAIL $(printf '%s' "$1" | tr '\n' ' ')" >> "$ASSERTIONS"
}

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
    # Structured per-assertion receipt (addendum §37): emitted from the
    # assertions log so downstream tools can count rows without parsing text.
    python3 - "$ASSERTIONS" "$OUT/assertions.json" <<'EOF' || true
import json, sys
rows = []
with open(sys.argv[1]) as fh:
    for line in fh:
        line = line.rstrip("\n")
        if not line:
            continue
        kind, _, label = line.partition(" ")
        if kind not in ("PASS", "FAIL"):
            continue
        rows.append({"assertion": label, "result": "PASS" if kind == "PASS" else "FAIL"})
with open(sys.argv[2], "w") as fh:
    json.dump(rows, fh, indent=2)
    # Trailing newline: the compatibility receipt's evidence dump cats this
    # file and then echoes the next section marker; without a final newline
    # the marker lands glued to the closing `]` and the parser loses the
    # section boundary (§37).
    fh.write("\n")
EOF
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
    #
    # The Wayland courts set COURT_NO_WM=1: sway IS the window manager there,
    # and an X11 WM would only reparent+decorate the wlroots x11-backend
    # window, offsetting the compositor's input coordinate space from the
    # window geometry the court queries (unmanaged windows sit at (0,0) with
    # no frame, so parent-relative == absolute == the pointer space — the
    # court's click math stays exact).
    if [ -n "${COURT_NO_WM:-}" ]; then
        echo "openbox skipped (sway is the window manager)" >>"$OUT/openbox.log"
    elif command -v openbox >/dev/null 2>&1; then
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" openbox >"$OUT/openbox.log" 2>&1 &
        sleep 1
        ok "openbox window manager started"
    else
        echo "openbox not installed; focus semantics degraded" >>"$OUT/openbox.log"
    fi
}

# ---------------------------------------------------------------------------
# The wlroots x11-backend window (the compositor output) for the Wayland
# courts. Its title ("wlroots - X11-N") is set via _NET_WM_NAME, but that
# property set FAILS on the dummy X server (the two ChangeProperty BadMatch
# errors at sway startup), so name-based search finds nothing. Without an X11
# WM the wlroots window is the ONLY child of the root window — the raw-tree
# fallback below is exact. Returns the window id, or empty.
# ---------------------------------------------------------------------------
find_compositor_window() {
    local win=""
    win=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        xdotool search --name 'wlroots' 2>/dev/null | head -1 || true)
    if [ -z "$win" ]; then
        # First child of the root window (header is 7 lines: the window-id
        # line, blank, root/parent lines, blank, then the child list).
        win=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xwininfo -root -children 2>/dev/null \
            | awk 'NR >= 7 && $1 ~ /^0x[0-9a-f]+$/ {print $1; exit}' || true)
    fi
    echo "$win"
}

# ---------------------------------------------------------------------------
# The wlroots compositor-output window's client geometry (position + size).
# This is the coordinate space every Wayland-court click lives in: unmanaged
# windows sit at (0,0) with no frame, so window-relative == pointer space.
# ---------------------------------------------------------------------------
compositor_geometry() { # compositor_geometry -> "X Y WIDTH HEIGHT" (or empty)
    local win geo x y w h
    win=$(find_compositor_window)
    [ -n "$win" ] || return 1
    geo=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        xdotool getwindowgeometry --shell "$win" 2>/dev/null || true)
    [ -n "$geo" ] || return 1
    eval "$geo"
    echo "$X $Y $WIDTH $HEIGHT"
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
#
# Each stage snapshots the raw X focus state so a failure can be attributed:
#  - the window was never activated (WM focus model, activation refused)
#  - the window was activated but the toolkit never reported focused:true
focus_target() {
    local win
    win=$(window_of ferrokey-test-target) || { bad "target window not found"; return 1; }
    [ -n "$win" ] || { bad "target window not found"; return 1; }
    focus_snapshot "before-activate"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        timeout 10 xdotool windowactivate "$win" 2>/dev/null || true
    sleep 0.5
    focus_snapshot "after-activate"
    # Click at the horizontal centre, ~2/3 down the window (the input field).
    eval "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$win")"
    local cx=$((X + WIDTH / 2)) cy=$((Y + HEIGHT * 2 / 3))
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$cx" "$cy" click 1
    sleep 0.5
    focus_snapshot "after-click"
}

# Record the raw X focus state at a named stage (rule 12: never infer support
# from window appearance alone; the snapshot is court evidence).
focus_snapshot() { # focus_snapshot <stage>
    local stage="$1" snap="$OUT/focus-snapshots.txt"
    {
        echo "== $stage at $(date +%s.%N)"
        echo "active-window: $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getactivewindow 2>&1 || true)"
        echo "input-focus:   $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowfocus 2>&1 || true)"
        echo "openbox:       $(pgrep -x openbox >/dev/null 2>&1 && echo running || echo DEAD)"
        echo "target windows:"
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name 'ferrokey-test-target' 2>/dev/null | while read -r w; do
            [ -n "$w" ] || continue
            echo "  $w: $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$w" 2>/dev/null | tr '\n' ' ')"
        done
    } >> "$snap"
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
    # Phase 3: the daemon is a constrained broker. The supervisor (`start`,
    # run briefly as root) parses the security-boundary config, the
    # bootstrap component creates the virtual keyboard, and the runtime
    # broker drops to the dedicated unprivileged `ferrokeyd` identity with
    # zero capabilities, NO_NEW_PRIVS and seccomp. The config must be
    # root-owned (the daemon refuses user-writable config while privileged,
    # §45).
    sudo chown root:root "$PAYLOAD/fixtures/ferrokeyd.yaml"
    sudo chmod 0644 "$PAYLOAD/fixtures/ferrokeyd.yaml"
    sudo -u root env RUST_LOG=info \
        "$PAYLOAD/bin/ferrokeyd" start --config "$PAYLOAD/fixtures/ferrokeyd.yaml" \
        >"$OUT/ferrokeyd.log" 2>&1 &
    FERROKEYD_PID=$!
    sleep 2
    if [ -S /run/ferrokeyd/ferrokeyd.sock ]; then
        ok "ferrokeyd listening"
    else
        bad "ferrokeyd did not start"
        cat "$OUT/ferrokeyd.log"
        finish_court FAIL "court" "ferrokeyd-start"
    fi
}

# The runtime broker pid (not the supervisor). bootstrap.rs execs the serve
# child via /proc/self/exe, so its argv[0] is "/proc/self/exe serve …"; the
# supervisor's argv[0] is the configured binary path ("…/ferrokeyd start …").
ferrokeyd_serve_pid() {
    pgrep -f "proc/self/exe serve" 2>/dev/null | head -1 || true
}

start_ferrokey() {
    # The UI is fully unprivileged and the courts already run as the court
    # user, so no sudo here: `$!` must be the actual ferrokey pid (a sudo
    # wrapper would fork the real process, and killing the wrapper would
    # orphan the UI with its daemon connection still open).
    local config="${1:-$PAYLOAD/fixtures/ferrokey.yaml}"
    env DISPLAY="$DISPLAY" WAYLAND_DISPLAY="" \
        XDG_RUNTIME_DIR=/tmp/court-runtime \
        "$PAYLOAD/bin/ferrokey" --config "$config" \
        >"$OUT/ferrokey.log" 2>&1 &
    FERROKEY_PID=$!
    sleep 3
    if kill -0 "$FERROKEY_PID" 2>/dev/null; then
        ok "ferrokey UI running (pid $FERROKEY_PID, config ${config##*/})"
    else
        bad "ferrokey UI exited"
        cat "$OUT/ferrokey.log"
        finish_court FAIL "phase" "ferrokey-start"
    fi
}

# ---------------------------------------------------------------------------
# Real-application targets (rules 50-55): browsers, Electron, SDL, terminal.
# These run BELOW the OSK (the OSK window occupies the top-left of the dummy
# 1280x720 screen; the full view is 1160x460), so field clicks never land on
# the OSK and vice versa.
# ---------------------------------------------------------------------------
start_http_server() { # start_http_server <dir> <port>
    sudo -u "$COURT_USER" python3 -m http.server "$2" --bind 127.0.0.1 --directory "$1" \
        >"$OUT/http-$2.log" 2>&1 &
    HTTP_PID=$!
    sleep 1
}

start_firefox() { # start_firefox <url>
    local profile=/tmp/ff-profile
    rm -rf "$profile" && mkdir -p "$profile"
    # Skip first-run so the URL tab is the active one (deterministic title).
    cat > "$profile/prefs.js" <<'EOF'
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionPolicyBypassNotification", true);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.page", 1);
EOF
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" MOZ_DISABLE_CONTENT_SANDBOX=1 \
        firefox-esr -no-remote -profile "$profile" "$1" >"$OUT/firefox.log" 2>&1 &
    FIREFOX_PID=$!
}

start_chromium() { # start_chromium <url>
    local profile=/tmp/chromium-profile
    rm -rf "$profile"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        chromium --no-sandbox --disable-gpu --disable-dev-shm-usage \
        --no-first-run --no-default-browser-check --user-data-dir="$profile" \
        "$1" >"$OUT/chromium.log" 2>&1 &
    CHROMIUM_PID=$!
}

start_electron() { # start_electron <app-dir>
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" TARGET_SOCKET="$TARGET_SOCKET" \
        /opt/electron/electron "$1" --no-sandbox --disable-gpu \
        >"$OUT/electron.log" 2>&1 &
    ELECTRON_PID=$!
}

start_sdl() {
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" TARGET_SOCKET="$TARGET_SOCKET" \
        SDL_AUDIODRIVER=dummy SDL_VIDEODRIVER=x11 \
        "$PAYLOAD/bin/ferrokey-test-target-sdl" >"$OUT/target-sdl.log" 2>&1 &
    TARGET_PID=$!
    wait_target_ready
}

start_xterm_target() { # start_xterm_target <title> <command...>
    # Optional xterm resource overrides (e.g. the terminal court's
    # Ctrl+Shift+V paste translation) via XTERM_XRM — mirrors the standard
    # modern-terminal paste binding that VTE-based terminals ship by default.
    local title="$1"
    shift
    local xrm=()
    [ -n "${XTERM_XRM:-}" ] && xrm=(-xrm "$XTERM_XRM")
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        xterm -title "$title" -geometry 100x12+100+480 "${xrm[@]}" -e "$@" \
        >"$OUT/xterm.log" 2>&1 &
    XTERM_PID=$!
}

# Window discovery / state (xdotool).
#
# Pick the LARGEST matching window: toolkits (GTK/GDK, Qt, Electron) create
# tiny hidden helper windows whose names match the same pattern, and `head
# -1` often lands on one — clicks/activations then miss the real window.
window_of() { # window_of <name-pattern> -> window id
    local best="" best_area=0
    while read -r win; do
        [ -n "$win" ] || continue
        local area
        area=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$win" 2>/dev/null \
            | awk -F= '/^(WIDTH|HEIGHT)=/ { v[$1]=$2 } END { print v["WIDTH"] * v["HEIGHT"] }')
        if [ -n "$area" ] && [ "$area" -gt "$best_area" ]; then
            best="$win"
            best_area=$area
        fi
    done < <(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name "$1" 2>/dev/null)
    echo "$best"
}

window_title() { # window_title <name-pattern> -> current title
    local win
    win=$(window_of "$1") || return 1
    [ -n "$win" ] || return 1
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowname "$win" 2>/dev/null || true
}

wait_window_name() { # wait_window_name <name-pattern> <timeout-s>
    local pat="$1" timeout="${2:-30}"
    for _ in $(seq 1 $((timeout * 2))); do
        [ -n "$(window_of "$pat")" ] && return 0
        sleep 0.5
    done
    return 1
}

wait_title() { # wait_title <window-pattern> <expected-fragment> <timeout-s>
    local pat="$1" want="$2" timeout="${3:-10}"
    for _ in $(seq 1 $((timeout * 2))); do
        if window_title "$pat" 2>/dev/null | grep -Fq "$want"; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

# Click the exact center of one of the page's editable fields.
#
# The browser court page reports its layout in the window title as
#   |GEO|<innerHeight>,<t>,<a>,<c>
# where t/a/c are VIEWPORT-relative y-centers of the text field, textarea
# and contenteditable. The court derives the browser chrome as
# window_height − innerHeight and clicks at Y + chrome + center. (The page
# must NOT add chrome itself: window.outerHeight includes the WM frame
# title bar, so page-computed chrome is off by the frame size and clicks
# land one field too low.)
browser_click_field() { # browser_click_field <window-pattern> <field: text|area|ce>
    local win
    win=$(window_of "$1") || return 1
    [ -n "$win" ] || return 1
    local title
    title=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        xdotool getwindowname "$win" 2>/dev/null || true)
    local geo
    geo=$(echo "$title" | sed -n 's/.*|GEO|\([0-9,]*\).*/\1/p')
    if [ -z "$geo" ]; then
        bad "browser: no |GEO| in window title: '$title'"
        return 1
    fi
    local ih=${geo%%,*} rest=${geo#*,}
    local fy=${rest%%,*} rest2=${rest#*,}
    local ay=${rest2%%,*} cy=${rest2#*,}
    local center="$fy"
    case "$2" in
        area) center="$ay" ;;
        ce)   center="$cy" ;;
    esac
    eval "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$win")"
    local chrome=$((HEIGHT - ih))
    local x=$((X + WIDTH / 2))
    local absy=$((Y + chrome + center))
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        xdotool mousemove "$x" "$absy" click 1
    sleep 0.5
}

# Click at a percentage of the named window (activate first). Never --sync:
# activation can block forever on an unresponsive target (browser courts).
click_fraction() { # click_fraction <window-pattern> <x-percent> <y-percent>
    local win
    win=$(window_of "$1") || return 1
    [ -n "$win" ] || return 1
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        timeout 10 xdotool windowactivate "$win" 2>/dev/null || true
    eval "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$win")"
    local x=$((X + WIDTH * $2 / 100)) y=$((Y + HEIGHT * $3 / 100))
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$x" "$y" click 1
    sleep 0.5
}

# Move + resize a target window so it sits entirely below the OSK.
#
# First unmaximize: browsers often start maximized, and xdotool's windowmove
# is silently ignored on maximized windows (its --sync then blocks forever
# waiting for a move the WM refuses — which hung the firefox court). wmctrl
# sends the proper EWMH _NET_WM_STATE remove; without it, degrade to plain
# xdotool (never --sync). All ops are non-blocking: a hung court is worse
# than an unmoved window.
position_target_below_osk() { # position_target_below_osk <window-pattern> <width> <height>
    local win
    win=$(window_of "$1") || return 1
    [ -n "$win" ] || return 1
    if command -v wmctrl >/dev/null 2>&1; then
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
            timeout 10 wmctrl -i -r "$win" -b remove,maximized_vert,maximized_horz \
            2>/dev/null || true
        sleep 0.5
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
            timeout 10 wmctrl -i -r "$win" -e 0,100,470,"$2","$3" \
            2>/dev/null || true
    else
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
            timeout 10 xdotool windowsize "$win" "$2" "$3" 2>/dev/null || true
        sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
            timeout 10 xdotool windowmove "$win" 100 470 2>/dev/null || true
    fi
    sleep 1
}

# ---------------------------------------------------------------------------
# OSK key geometry (rule 13): click a physical key by name using xdotool.
# The OSK window is at (0,0); its size and key layout depend on the active
# view (compact: 920x342, full: 1160x460). Courts set OSK_VIEW to target
# the full-desktop view.
# ---------------------------------------------------------------------------
click_osk_key() { # click_osk_key <key-name> [button: 1|2|3]
    click_osk_key_button "$1" "${2:-1}"
}

click_osk_key_button() { # click_osk_key_button <key-name> <button>
    local pos
    pos=$(osk_key_pos "$1") || {
        bad "unknown OSK key $1"
        return 1
    }
    local x="${pos%,*}" y="${pos#*,}" btn="$2"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$x" "$y" click "$btn"
    sleep 0.3
}

hold_osk_key() { # hold_osk_key <key-name> <hold-ms>
    local pos
    pos=$(osk_key_pos "$1") || return 1
    local x="${pos%,*}" y="${pos#*,}"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$x" "$y" mousedown 1
    sleep "$(awk "BEGIN{print $2/1000}")"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
    sleep 0.3
}

# The on-screen center of a key in the active view (default: compact).
osk_key_pos() { # osk_key_pos <key-name>
    local view="${OSK_VIEW:-compact}"
    python3 "$PAYLOAD/courts/osk-geometry.py" --view "$view" "$1"
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
