#!/usr/bin/env bash
# CHROMIUM.001-006 (rule 51): real editable fields in a Chromium-family
# browser. Electron support is NOT inferred from this court (rule 53) — the
# same sequence runs against the Electron target separately.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"
source "$(dirname "$0")/../browser-tests.sh"

export OSK_VIEW=full
run_browser_court chromium

finish_court "court" "chromium"
