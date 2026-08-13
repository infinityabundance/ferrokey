#!/usr/bin/env bash
# TERMINAL.001-006 (rule 52): a real terminal emulator (xterm) as the target.
#
# A probe inside the terminal puts the PTY into raw mode and logs every byte
# the OSK delivers, so exact key behaviour is asserted as byte sequences:
#   abc → 61 62 63, Left → 1b 5b 44, Home → 1b 5b 48, F5 → 1b 5b 31 35 7e,
#   Ctrl+C → 03, Ctrl+D → 04, Ctrl+L → 0c, Ctrl+Shift+V → the pasted text.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

export OSK_VIEW=full
PROBE=/tmp/term-probe.out
rm -f "$PROBE"

# The standard modern-terminal paste binding (VTE-based terminals ship this
# by default): Ctrl+Shift+V → insert CLIPBOARD. Plain xterm has no binding
# for it — an unbound Ctrl+Shift+V falls through to the raw Ctrl+V byte
# (0x16), which would make the paste assertion meaningless. So the court
# gives xterm the same translation a desktop terminal has, then proves the
# OSK delivers the chord (rule 52).
export XTERM_XRM='XTerm.vt100.translations: #override Ctrl Shift <Key>V: insert-selection(CLIPBOARD)'

start_xorg
start_ferrokeyd
start_xterm_target ferrokey-term-target \
    python3 "$PAYLOAD/courts/terminal/term-probe.py" "$PROBE"
start_ferrokey "$PAYLOAD/fixtures/ferrokey-full.yaml"

# The probe writes "ready" once it holds the PTY in raw mode.
for _ in $(seq 1 100); do
    grep -q '^ready$' "$PROBE" 2>/dev/null && break
    sleep 0.5
done
if grep -q '^ready$' "$PROBE" 2>/dev/null; then
    ok "terminal probe ready"
else
    bad "terminal probe never started"
    tail -10 "$OUT/xterm.log"
    finish_court FAIL "phase" "terminal-start"
fi

# Focus the xterm window (positioned below the OSK by its geometry). No
# --sync: activation can block forever if the WM delays the acknowledge.
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" timeout 10 xdotool windowactivate \
    "$(window_of ferrokey-term-target)"
sleep 0.5

# ── TERMINAL.001: plain text ───────────────────────────────────────────────
# (No focus_before/focus_after here: the terminal court has no focus-
# reporting target — the raw-PTY probe is the oracle. Focus retention is
# proven by TERMINAL.005, where the probe keeps receiving keys.)
click_osk_key a
click_osk_key b
click_osk_key c
sleep 1
if grep -q "61 62 63" "$PROBE" 2>/dev/null; then
    ok "terminal: typed 'abc'"
else
    bad "terminal: 'abc' missing; probe: $(cat "$PROBE" | tail -3)"
fi

# ── TERMINAL.002: navigation + function keys ───────────────────────────────
click_osk_key left
sleep 0.5
if grep -q "1b 5b 44" "$PROBE" 2>/dev/null; then
    ok "terminal: Left arrow (ESC [ D)"
else
    bad "terminal: Left arrow missing"
fi
click_osk_key up
sleep 0.5
if grep -q "1b 5b 41" "$PROBE" 2>/dev/null; then
    ok "terminal: Up arrow (ESC [ A)"
fi
click_osk_key home
sleep 0.5
if grep -q "1b 5b 48" "$PROBE" 2>/dev/null; then
    ok "terminal: Home (ESC [ H)"
fi
click_osk_key f5
sleep 0.5
if grep -q "1b 5b 31 35 7e" "$PROBE" 2>/dev/null; then
    ok "terminal: F5 (ESC [ 1 5 ~)"
else
    bad "terminal: F5 missing"
fi

# ── TERMINAL.003: control characters (raw mode) ────────────────────────────
click_osk_key left-ctrl
click_osk_key c
sleep 0.5
if grep -qE "(^| )03( |$)" "$PROBE" 2>/dev/null; then
    ok "terminal: Ctrl+C delivered 0x03"
else
    bad "terminal: Ctrl+C missing"
fi
click_osk_key left-ctrl
click_osk_key d
sleep 0.5
if grep -qE "(^| )04( |$)" "$PROBE" 2>/dev/null; then
    ok "terminal: Ctrl+D delivered 0x04"
fi
click_osk_key left-ctrl
click_osk_key l
sleep 0.5
if grep -qE "(^| )0c( |$)" "$PROBE" 2>/dev/null; then
    ok "terminal: Ctrl+L delivered 0x0c"
fi

# ── TERMINAL.004: Ctrl+Shift+V pastes the CLIPBOARD ────────────────────────
# xclip forks a clipboard-owner process that inherits the SSH session's
# stdout/stderr; redirect those so the court's ssh channel closes. stdin must
# stay the pipe from echo (a </dev/null would empty the selection!).
echo -n "paste-me" | sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
    xclip -selection clipboard >/dev/null 2>&1
sleep 0.5
click_osk_key left-ctrl    # tap → latch
click_osk_key left-shift   # tap → latch (both latched)
click_osk_key v
sleep 1
if grep -q "70 61 73 74 65 2d 6d 65" "$PROBE" 2>/dev/null; then
    ok "terminal: Ctrl+Shift+V pasted 'paste-me'"
else
    bad "terminal: Ctrl+Shift+V paste missing; probe tail: $(tail -3 "$PROBE")"
fi

# ── TERMINAL.005: focus retention ─────────────────────────────────────────
# Two independent proofs: (1) the probe still receives keys (input path
# intact), and (2) X itself reports keyboard focus on the xterm window —
# never on the OSK (the no-focus contract).
click_osk_key z
sleep 0.5
if grep -q "7a" "$PROBE" 2>/dev/null; then
    ok "terminal: probe still receiving keys (xterm kept focus)"
else
    bad "terminal: input stopped arriving (focus lost?)"
fi
FOCUSED=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
    xdotool getwindowfocus getwindowname 2>/dev/null || true)
if echo "$FOCUSED" | grep -q ferrokey-term-target; then
    ok "terminal: X keyboard focus is on xterm (never the OSK)"
else
    bad "terminal: keyboard focus lost; focused: '$FOCUSED'"
fi

finish_court "court" "terminal"
