#!/usr/bin/env bash
# FIREFOX.001-006 (rule 51): real editable fields in a real Firefox browser.
#
# The OSK (full view) types into Firefox's text field, textarea and
# contenteditable; Backspace, arrow navigation, Ctrl+A, Ctrl+Home/End all
# behave as on a physical keyboard; the browser keeps keyboard focus the
# whole time (the OSK never takes it).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"
source "$(dirname "$0")/../browser-tests.sh"

export OSK_VIEW=full
run_browser_court firefox

finish_court "court" "firefox"
