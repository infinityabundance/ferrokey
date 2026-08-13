#!/usr/bin/env bash
# TERM.PTY / TERM.KEYS / TERM.CTRL / TERM.ALT / TERM.NAV / TERM.RESIZE /
# TERM.SCROLLBACK / TERM.VIEWPORT / TERM.ALTSCREEN / TERM.SELECTION /
# TERM.SHELL / TERM.NO_UINPUT / TERM.SECURITY / TERM.RESTART / TERM.TUI
# (Phase 3 addendum #2, §87–§105, §120).
#
# The embedded terminal workspace: the OSK drives a REAL PTY directly —
# no ferrokeyd, no /dev/uinput, no compositor focus involvement. The PTY
# child is the deterministic pty-oracle probe (§99), which reports every
# byte it receives, its window size and SIGWINCH over a Unix socket, and
# can script terminal-application responses (clear, DSR, output, alternate
# screen) to close the OSK → PTY → parser → response → child loop.
#
# All commands are typed through the actual OSK with xdotool — bytes are
# never injected directly (§90).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

export OSK_VIEW=terminal
KEYBOARD_H=342        # terminal view height (physical px)
PANE_H=220
WIN_W=920
WIN_H=$((KEYBOARD_H + PANE_H))
export RUST_LOG=debug

ORACLE_SOCK=/tmp/pty-oracle.sock
ORACLE_LOG="$OUT/oracle.log"
rm -f "$ORACLE_SOCK" "$ORACLE_LOG"

ORACLE_FIXTURE="$OUT/ferrokey-terminal-oracle.yaml"
cat > "$ORACLE_FIXTURE" <<EOF
layout: us
view: terminal
width: $WIN_W
height: $KEYBOARD_H
terminal:
  enabled: true
  pane_height: $PANE_H
  font_size_px: 16
  scrollback_lines: 10000
  shell: $PAYLOAD/bin/pty-oracle
destination: terminal
repeat:
  enabled: true
  delay_ms: 500
  cadence_ms: 30
EOF

# The concatenated hex of every input event the oracle received so far.
oracle_hex() {
    python3 - "$ORACLE_LOG" <<'PYEOF'
import json, sys
total = ""
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    try:
        ev = json.loads(line)
    except Exception:
        continue
    if ev.get("event") == "input":
        total += ev["hex"]
print(total, end="")
PYEOF
}

# Assert the oracle's accumulated input hex ends with $2.
assert_oracle_suffix() { # assert_oracle_suffix <label> <expected-hex>
    local hex
    hex=$(oracle_hex)
    if [[ "$hex" == *"$2" ]]; then
        ok "$1"
    else
        bad "$1 (expected suffix $2; got ...$hex)"
    fi
}

# Assert the oracle's accumulated input hex contains $2 (as a substring).
assert_oracle_contains() { # assert_oracle_contains <label> <hex>
    local hex
    hex=$(oracle_hex)
    if [[ "$hex" == *"$2"* ]]; then
        ok "$1"
    else
        bad "$1 (missing $2 in ...$hex)"
    fi
}

# Type one OSK character and let the oracle drain it.
type_char() { # type_char <key-name>
    click_osk_key "$1"
    sleep 0.35
}

# Type a whole command word (letters, dots, digits) via the OSK.
type_command() { # type_command <command-text>
    local text="$1" i
    for ((i = 0; i < ${#text}; i++)); do
        local ch="${text:i:1}"
        case "$ch" in
            '.') type_char dot ;;
            *)   type_char "$ch" ;;
        esac
    done
    type_char enter
}

kill_ferrokey() {
    if [ -n "${FERROKEY_PID:-}" ]; then
        kill "$FERROKEY_PID" 2>/dev/null || true
        wait "$FERROKEY_PID" 2>/dev/null || true
        sleep 1
    fi
}

start_xorg

# ── TERM.NO_UINPUT.001 (mandatory §88): no daemon, no device ──────────────
if pgrep -x ferrokeyd >/dev/null 2>&1; then
    bad "TERM.NO_UINPUT: ferrokeyd must not run in terminal mode"
else
    ok "TERM.NO_UINPUT: ferrokeyd not running"
fi
if grep -qi ferrokey /proc/bus/input/devices 2>/dev/null; then
    bad "TERM.NO_UINPUT: a ferrokey input device already exists"
else
    ok "TERM.NO_UINPUT: no ferrokey input device before start"
fi

nohup python3 "$PAYLOAD/courts/terminal-workspace/oracle-listen.py" "$ORACLE_SOCK" \
    > "$ORACLE_LOG" 2>"$OUT/oracle.err" &
ORACLE_LISTENER=$!

export PTY_ORACLE_SOCKET="$ORACLE_SOCK"
start_ferrokey "$ORACLE_FIXTURE"

ORACLE_UP=0
for _ in $(seq 1 100); do
    if grep -q '"event":"start"' "$ORACLE_LOG" 2>/dev/null; then
        ORACLE_UP=1
        break
    fi
    sleep 0.5
done
if [ "$ORACLE_UP" = 1 ]; then
    ok "terminal oracle started via the OSK → PTY path"
else
    bad "terminal oracle never started"
    tail -30 "$OUT/ferrokey.log" || true
    finish_court FAIL "phase" "terminal-oracle-start"
fi
if grep -q '"event":"winsize"' "$ORACLE_LOG"; then
    ok "TERM.PTY: oracle reported a real window size"
else
    bad "TERM.PTY: no winsize reported"
fi
if grep -qi ferrokey /proc/bus/input/devices 2>/dev/null; then
    bad "TERM.NO_UINPUT: ferrokey created an input device in terminal mode"
else
    ok "TERM.NO_UINPUT: no ferrokey input device exists in terminal mode"
fi

# ── TERM.PTY.001: bytes flow OSK → core → encoder → PTY → child ──────────
type_command "clr"
sleep 0.6
type_command "dsr"
for _ in $(seq 1 40); do
    [[ "$(oracle_hex)" == *1b5b313b3152* ]] && break
    sleep 0.5
done
assert_oracle_contains "TERM.PTY.001: terminal answered DSR with cursor position ESC[1;1R" \
    "1b5b313b3152"
# The child writes "hello"; the terminal parses it into the grid, so the
# next cursor position is ESC[1;6R.
type_command "out.hello"
sleep 0.6
type_command "dsr"
for _ in $(seq 1 40); do
    [[ "$(oracle_hex)" == *1b5b313b3652* ]] && break
    sleep 0.5
done
assert_oracle_contains "TERM.PTY.001: terminal parsed child output (cursor advanced to col 6)" \
    "1b5b313b3652"

# ── TERM.SHORTCUT.001 (§55, §57): the shortcut row plays REAL key chords ─
# The Ctrl+C key must produce 0x03 through the core + encoder — never an
# internal shell-command macro.
click_osk_key "Ctrl+C"
sleep 0.5
assert_oracle_suffix "TERM.SHORTCUT.001: Ctrl+C chord produced 0x03" "03"
click_osk_key "Ctrl+D"
sleep 0.5
assert_oracle_suffix "TERM.SHORTCUT.001: Ctrl+D chord produced 0x04" "04"
click_osk_key "Ctrl+Z"
sleep 0.5
assert_oracle_suffix "TERM.SHORTCUT.001: Ctrl+Z chord produced 0x1a" "1a"

# ── TERM.KEYS.001 (§90): real OSK typing reaches the PTY byte-exact ───────
for k in e c h o space f e r r o k e y enter; do
    type_char "$k"
done
assert_oracle_suffix "TERM.KEYS.001: 'echo ferrokey' + Enter byte-exact" \
    "6563686f20666572726f6b65790d"

# ── TERM.CTRL.001 (§91): Ctrl chords are control bytes ────────────────────
click_osk_key left-ctrl
sleep 0.3
type_char c
assert_oracle_suffix "TERM.CTRL.001: Ctrl+C is 0x03" "03"
click_osk_key left-ctrl
sleep 0.3
type_char d
assert_oracle_suffix "TERM.CTRL.001: Ctrl+D is 0x04" "04"
click_osk_key left-ctrl
sleep 0.3
type_char l
assert_oracle_suffix "TERM.CTRL.001: Ctrl+L is 0x0c" "0c"
click_osk_key left-ctrl
sleep 0.3
type_char z
assert_oracle_suffix "TERM.CTRL.001: Ctrl+Z is 0x1a" "1a"

# ── TERM.ALT.001 (§92): Alt is an ESC prefix ──────────────────────────────
click_osk_key left-alt
sleep 0.3
type_char x
assert_oracle_suffix "TERM.ALT.001: Alt+X is ESC+x" "1b78"

# ── TERM.NAV.001 (§93): navigation keys, normal + application mode ────────
type_char up
assert_oracle_suffix "TERM.NAV.001: Up is ESC[A" "1b5b41"
type_char down
assert_oracle_suffix "TERM.NAV.001: Down is ESC[B" "1b5b42"
type_char left
assert_oracle_suffix "TERM.NAV.001: Left is ESC[D" "1b5b44"
type_char right
assert_oracle_suffix "TERM.NAV.001: Right is ESC[C" "1b5b43"
type_char home
assert_oracle_suffix "TERM.NAV.001: Home is ESC[H" "1b5b48"
type_char end
assert_oracle_suffix "TERM.NAV.001: End is ESC[F" "1b5b46"
# Application cursor mode: the child enables DECCKM; arrows become SS3.
type_command "appc.on"
sleep 0.6
type_char up
assert_oracle_suffix "TERM.NAV.001: Up in application mode is ESC OA" "1b4f41"
type_command "appc.off"
sleep 0.6

# ── TERM.FKEY.001 (§94): function keys ────────────────────────────────────
# NOTE: the terminal OSK has no function-key row (the compact/full views do,
# and the xterm TERMINAL court exercises F5 there); the encoder's f-key
# sequences (ESC OP, ESC[15~, ESC[24~, …) are asserted byte-exact by the
# key_encoder unit tests. §90 forbids injecting bytes the OSK cannot type.

# ── TERM.ALTSCREEN.001 (§97): alternate screen round trip ─────────────────
type_command "alt.on"
sleep 0.6
type_command "clr"
sleep 0.6
type_command "dsr"
for _ in $(seq 1 40); do
    [[ "$(oracle_hex)" == *1b5b313b3152* ]] && break
    sleep 0.5
done
assert_oracle_contains "TERM.ALTSCREEN.001: terminal responds inside the alternate screen" \
    "1b5b313b3152"
type_command "alt.off"
sleep 0.6

# ── TERM.SECURITY.001 (§105): hostile child output never breaks the terminal
type_command "hostile"
sleep 1.2
type_command "clr"
sleep 0.6
type_command "dsr"
for _ in $(seq 1 40); do
    [[ "$(oracle_hex)" == *1b5b313b3152* ]] && break
    sleep 0.5
done
assert_oracle_contains "TERM.SECURITY.001: terminal survived hostile escape flood" \
    "1b5b313b3152"

# ── TERM.SCROLLBACK.001 / TERM.VIEWPORT.001 (§96, §22–§23) ────────────────
type_command "flood"
sleep 1.5
WY=$((KEYBOARD_H + PANE_H - 60))
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove 600 "$WY" mousedown 1
sleep 0.2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove 600 $((WY - 120))
sleep 0.2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.5
if grep -q "terminal viewport scroll_up" "$OUT/ferrokey.log"; then
    ok "TERM.SCROLLBACK.001: pane drag scrolled into history"
else
    bad "TERM.SCROLLBACK.001: pane drag did not scroll"
fi
# The ↓ newest control returns to the live edge.
NEWEST_X=$((WIN_W - 104))
NEWEST_Y=$((WIN_H - 34))
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$NEWEST_X" "$NEWEST_Y" click 1
sleep 0.5
if grep -q "terminal viewport return_to_newest" "$OUT/ferrokey.log"; then
    ok "TERM.VIEWPORT.001: ↓ newest returned to the live edge"
else
    bad "TERM.VIEWPORT.001: ↓ newest did not fire"
fi

# ── TERM.RESIZE.001 (§95): window resize reaches the child via SIGWINCH ───
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool windowsize \
    "$(window_of ferrokey)" 1000 700 2>/dev/null || true
sleep 1.5
# 700 tall → pane 700-342 = 358 → rows 358/17 = 21; 1000 wide → cols 125.
if grep -q '"rows":21,"cols":125' "$ORACLE_LOG" 2>/dev/null; then
    ok "TERM.RESIZE.001: child received the resized PTY window (21x125)"
else
    bad "TERM.RESIZE.001: child did not receive a resize; oracle winsizes:"
    grep '"event":"winsize"' "$ORACLE_LOG" | tail -3 || true
fi

# ── TERM.NO_UINPUT.001 final ──────────────────────────────────────────────
if grep -qi ferrokey /proc/bus/input/devices 2>/dev/null; then
    bad "TERM.NO_UINPUT: input device appeared during terminal mode"
else
    ok "TERM.NO_UINPUT: zero input devices after full terminal interaction"
fi

# ── TERM.SHELL.001: a real shell runs, executes, and exits with status ────
# Preserve the oracle-phase app log first (the shell phase overwrites it).
cp "$OUT/ferrokey.log" "$OUT/ferrokey-oracle-phase.log" 2>/dev/null || true
kill_ferrokey
SHELL_FIXTURE="$OUT/ferrokey-terminal-shell.yaml"
cat > "$SHELL_FIXTURE" <<EOF
layout: us
view: terminal
width: $WIN_W
height: $KEYBOARD_H
terminal:
  enabled: true
  pane_height: $PANE_H
  font_size_px: 16
  scrollback_lines: 10000
  shell: /bin/sh
destination: terminal
EOF
start_ferrokey "$SHELL_FIXTURE"
sleep 2
for k in e x i t enter; do
    click_osk_key "$k"
    sleep 0.4
done
for _ in $(seq 1 60); do
    grep -q "terminal child exited: exited with status 0" "$OUT/ferrokey.log" && break
    sleep 0.5
done
if grep -q "terminal child exited: exited with status 0" "$OUT/ferrokey.log"; then
    ok "TERM.SHELL.001: real shell executed 'exit' and the broker observed status 0"
else
    bad "TERM.SHELL.001: shell exit status not observed"
    grep -i "terminal child" "$OUT/ferrokey.log" | tail -3 || true
fi

# ── TERM.RESTART.001 (§37–§39): the [Restart] control spawns a new session
RESTART_X=$((WIN_W - 104))
RESTART_Y=$((KEYBOARD_H + 16))
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$RESTART_X" "$RESTART_Y" click 1
sleep 2
if grep -q "terminal session started" "$OUT/ferrokey.log"; then
    ok "TERM.RESTART.001: [Restart] spawned a fresh terminal session"
else
    bad "TERM.RESTART.001: [Restart] did not respawn the session"
fi

# ── TERM.TUI.001 (§98): a real TUI (vim.tiny) as the PTY child ────────────
# vim refuses a broken PTY: it prints "Vim: Warning: Output is not to a
# terminal" and degrades. TERM.PTY already proved the PTY is real (winsize,
# raw mode); this phase proves a REAL terminal application — its alt-screen
# UI, cursor addressing and Ex-command loop — works over the OSK → encoder →
# PTY path, and that the broker reaps vim's OWN exit status after `:q!`.
# The wrapper execs vim so the child's exit code IS vim's exit code, and
# diverts vim's stderr to a court-readable file (the broken-terminal warning
# lands on stderr; the UI stays on stdout → the PTY).
kill_ferrokey
TUI_VIM="$OUT/tui-vim.sh"
TUI_VIM_ERR="$OUT/tui-vim.stderr"
cat > "$TUI_VIM" <<EOF
#!/bin/bash
: > $TUI_VIM_ERR
exec /usr/bin/vim.tiny -N -u NONE -i NONE -n 2>>$TUI_VIM_ERR
EOF
chmod +x "$TUI_VIM"
TUI_FIXTURE="$OUT/ferrokey-terminal-tui.yaml"
cat > "$TUI_FIXTURE" <<EOF
layout: us
view: terminal
width: $WIN_W
height: $KEYBOARD_H
terminal:
  enabled: true
  pane_height: $PANE_H
  font_size_px: 16
  scrollback_lines: 10000
  shell: $TUI_VIM
destination: terminal
EOF
start_ferrokey "$TUI_FIXTURE"
for _ in $(seq 1 60); do
    grep -q "terminal session started" "$OUT/ferrokey.log" && break
    sleep 0.5
done
# The wrapper creates the stderr file the moment the session spawns; give
# vim time to initialize its UI (alt screen, size query → the terminal's
# DSR answer).
VIM_READY=0
for _ in $(seq 1 40); do
    [ -f "$TUI_VIM_ERR" ] && VIM_READY=1 && break
    sleep 0.5
done
sleep 2
if [ "$VIM_READY" = 1 ] && ! grep -qi "not to a terminal\|E558" "$TUI_VIM_ERR" 2>/dev/null; then
    ok "TERM.TUI.001: vim accepted the PTY as a real terminal"
else
    bad "TERM.TUI.001: vim rejected the PTY; stderr: $(tail -5 "$TUI_VIM_ERR" 2>/dev/null | tr '\n' ' ')"
fi
# :q! through the OSK: ':' = latch Shift + semicolon, '!' = latch Shift + 1.
click_osk_key left-shift
sleep 0.3
type_char semicolon
type_char q
click_osk_key left-shift
sleep 0.3
type_char "1"
type_char enter
for _ in $(seq 1 60); do
    grep -q "terminal child exited: exited with status 0" "$OUT/ferrokey.log" && break
    sleep 0.5
done
if grep -q "terminal child exited: exited with status 0" "$OUT/ferrokey.log"; then
    ok "TERM.TUI.001: vim ran on the PTY and quit cleanly via the OSK ':q!'"
else
    bad "TERM.TUI.001: vim did not exit cleanly"
    grep -i "terminal child" "$OUT/ferrokey.log" | tail -3 || true
    tail -10 "$TUI_VIM_ERR" 2>/dev/null || true
fi

finish_court "court" "terminal-workspace"
