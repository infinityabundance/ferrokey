#!/usr/bin/env bash
# Shared browser-court sequence (rules 50-51): real editable fields in a real
# browser. The browser must receive every key the OSK injects, keep keyboard
# focus the whole time, and update the test page's title — a machine-readable
# oracle (rule 17: no screenshots).
#
# The court runs the FULL view (arrows / Home / End live there); the browser
# window is parked below the OSK (100,470), so field clicks and OSK clicks
# never overlap. The active view + fixture are the caller's responsibility.

# run_browser_court <firefox|chromium>
run_browser_court() {
    local browser="$1"
    # The page title is the machine-readable oracle; it always starts with
    # "FERROKEY|" (the field separator pipe). The OSK window is titled
    # "Ferrokey Virtual Keyboard" — a case-insensitive `xdotool search
    # --name FERROKEY` matches BOTH once the OSK starts, so the pattern must
    # include the pipe the OSK title never has. `\|` escapes the regex pipe.
    local title="FERROKEY"        # human-readable label / title fragment
    local title_pat='FERROKEY\|'  # xdotool search pattern (regex)

    start_xorg
    start_recorder
    start_ferrokeyd
    start_http_server "$PAYLOAD/courts" 8000

    if [ "$browser" = "firefox" ]; then
        start_firefox "http://127.0.0.1:8000/browser-page.html"
    else
        start_chromium "http://127.0.0.1:8000/browser-page.html"
    fi

    if wait_window_name "$title_pat" 120; then
        ok "$browser: page loaded (title ${title}|||)"
    else
        bad "$browser: window never appeared"
        tail -30 "$OUT/$browser.log"
        finish_court FAIL "phase" "$browser-start"
    fi

    # Park the browser below the OSK, then start the OSK itself.
    position_target_below_osk "$title_pat" 900 240
    start_ferrokey "$PAYLOAD/fixtures/ferrokey-full.yaml"

    # ── FIREFOX/CHROMIUM.001: plain text + Backspace ─────────────────────
    browser_click_field "$title_pat" text
    click_osk_key h
    click_osk_key i
    if wait_title "$title_pat" 'FERROKEY|hi||' 20; then
        ok "$browser: typed 'hi' into the text field"
    else
        bad "$browser: 'hi' not typed; title: $(window_title "$title_pat")"
    fi
    assert_focus_on_target "$browser" "$title_pat"

    click_osk_key backspace
    if wait_title "$title_pat" 'FERROKEY|h||' 20; then
        ok "$browser: Backspace deleted a character"
    else
        bad "$browser: Backspace failed; title: $(window_title "$title_pat")"
    fi

    # ── .002: navigation key (Left arrow) ─────────────────────────────────
    click_osk_key a
    click_osk_key b
    click_osk_key left
    click_osk_key x
    if wait_title "$title_pat" 'FERROKEY|haxb||' 20; then
        ok "$browser: Left arrow moved the caret (ab + Left + x → axb)"
    else
        bad "$browser: arrow navigation failed; title: $(window_title "$title_pat")"
    fi

    # ── .003: textarea with Enter ─────────────────────────────────────────
    browser_click_field "$title_pat" area   # the textarea
    click_osk_key l
    click_osk_key "1"
    click_osk_key enter
    click_osk_key l
    click_osk_key "2"
    if wait_title "$title_pat" 'FERROKEY|haxb|l1\nl2|' 20; then
        ok "$browser: textarea received l1 + Enter + l2"
    else
        bad "$browser: textarea/Enter failed; title: $(window_title "$title_pat")"
    fi

    # ── .004: contenteditable ─────────────────────────────────────────────
    browser_click_field "$title_pat" ce    # the contenteditable
    click_osk_key e
    click_osk_key d
    click_osk_key i
    click_osk_key t
    if wait_title "$title_pat" 'FERROKEY|haxb|l1\nl2|edit' 20; then
        ok "$browser: contenteditable received 'edit'"
    else
        bad "$browser: contenteditable failed; title: $(window_title "$title_pat")"
    fi

    # ── .005: browser shortcut Ctrl+A (select all) ────────────────────────
    browser_click_field "$title_pat" text  # back to the text field
    click_osk_key left-ctrl            # tap → latch Ctrl
    click_osk_key a                    # Ctrl+A: select all
    click_osk_key z                    # replace selection
    if wait_title "$title_pat" 'FERROKEY|z|l1\nl2|edit' 20; then
        ok "$browser: Ctrl+A selected all and typing replaced it"
    else
        bad "$browser: Ctrl+A failed; title: $(window_title "$title_pat")"
    fi
    assert_focus_on_target "$browser" "$title_pat"

    # ── .006: Ctrl+Home / Ctrl+End in the textarea ───────────────────────
    browser_click_field "$title_pat" area  # the textarea
    click_osk_key left-ctrl
    click_osk_key end                  # Ctrl+End: caret to end of text
    click_osk_key a
    click_osk_key b
    if wait_title "$title_pat" 'FERROKEY|z|l1\nl2ab|' 20; then
        ok "$browser: Ctrl+End moved the caret to the text end"
    else
        bad "$browser: Ctrl+End failed; title: $(window_title "$title_pat")"
    fi
    click_osk_key left-ctrl
    click_osk_key home                 # Ctrl+Home: caret to start
    click_osk_key x
    if wait_title "$title_pat" 'FERROKEY|z|xl1\nl2ab|' 20; then
        ok "$browser: Ctrl+Home moved the caret to the text start"
    else
        bad "$browser: Ctrl+Home failed; title: $(window_title "$title_pat")"
    fi
    click_osk_key left-ctrl
    click_osk_key end
    click_osk_key y
    if wait_title "$title_pat" 'FERROKEY|z|xl1\nl2aby|' 20; then
        ok "$browser: Ctrl+End again + typing appended"
    else
        bad "$browser: final Ctrl+End failed; title: $(window_title "$title_pat")"
    fi

    # ── Focus evidence: the browser (never the OSK) owns keyboard focus ──
    assert_focus_on_target "$browser" "$title_pat"
}

# The X focus must be on the browser window, never on the OSK (the no-focus
# contract). The title-based text assertions above already prove the keys
# landed; this is the direct focus check.
assert_focus_on_target() { # assert_focus_on_target <label> <title-pattern>
    local label="$1" pat="$2"
    local focused
    focused=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" \
        xdotool getwindowfocus getwindowname 2>/dev/null || true)
    if echo "$focused" | grep -q "$pat"; then
        ok "$label: keyboard focus is on the target (never the OSK)"
    else
        bad "$label: keyboard focus lost; focused: '$focused'"
    fi
}
